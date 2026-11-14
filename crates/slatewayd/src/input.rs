// SPDX-License-Identifier: GPL-3.0-or-later

//! The input path (docs/ROADMAP.md M1, "Touch as pointer"): tablet
//! input batches flow from the session loop into a dedicated OS
//! thread that owns the platform [`PointerInjector`]. Injection calls
//! block (D-Bus on Linux, user32 on Windows), so they never run on
//! the async session task.
//!
//! Backpressure (docs/PROTOCOL.md, "Backpressure": input is never
//! dropped, move events coalesce when the queue grows): the worker
//! drains everything available and collapses runs of moves for the
//! same contact to the newest event, so a stroke that outpaces the
//! injector lands at its latest position. Downs and ups are always
//! delivered.
//!
//! Contact policy (user-approved M1): the first contact to go down is
//! primary and drives the pointer until it lifts. A second contact
//! (two-finger touch) is ignored entirely — no pointer jumps, no
//! stuck buttons; its events are counted as coalesced. Pen rides the
//! pointer path for M1 (the injector treats pen like finger), which
//! is enough for tap-and-drag until M3's real pen devices.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use slateway_platform::input::PointerInjector;
use slateway_proto::input::{EventKind, InputEvent, parse_batch};

/// How long the worker waits between shutdown checks while idle. The
/// session loop ends by dropping the sender; this only bounds the
/// wait in tests and panics.
const IDLE_POLL: std::time::Duration = std::time::Duration::from_millis(200);

/// Counters for the session report.
#[derive(Debug, Default)]
pub struct InputCounters {
    /// Events received from the tablet (all contacts).
    pub events_in: AtomicU64,
    /// Events injected into the OS (primary-contact events).
    pub events_injected: AtomicU64,
    /// Events collapsed at the coalescer or ignored by the contact
    /// policy (second contacts, hover).
    pub events_coalesced: AtomicU64,
    /// Events the injector rejected (logged once, not fatal).
    pub injection_errors: AtomicU64,
}

/// Command channel: the session loop forwards every parsed event.
pub type InputTx = std::sync::mpsc::Sender<InputEvent>;

/// Parses an input-channel payload into events. Returns an error
/// string for malformed batches (they are authenticated data: a bug,
/// not an attack — the session logs and moves on).
pub fn parse_input_batch(payload: &[u8]) -> Result<Vec<InputEvent>, String> {
    let mut events = Vec::with_capacity(payload.len() / slateway_proto::input::EVENT_LEN);
    for event in parse_batch(payload) {
        events.push(event.map_err(|e| e.to_string())?);
    }
    Ok(events)
}

/// Runs the input worker on the calling thread until the session's
/// sender is gone or the stop flag is set. Returns the counters for
/// the session report.
pub fn run_input_worker(
    mut injector: Box<dyn PointerInjector>,
    rx: std::sync::mpsc::Receiver<InputEvent>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) -> InputCounters {
    let counters = InputCounters::default();
    // The contact that went down first and is still down; None when
    // no contact is active. Only its events inject.
    let mut primary: Option<u8> = None;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let first = match rx.recv_timeout(IDLE_POLL) {
            Ok(event) => {
                counters.events_in.fetch_add(1, Ordering::Relaxed);
                event
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            // Session over: every sender dropped.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // Collapse runs of same-contact moves while the queue is
        // non-empty (docs/PROTOCOL.md "Backpressure"): the injector
        // sees one event per position burst.
        let mut current = first;
        let mut collapsed: Option<InputEvent> = None;
        loop {
            match rx.try_recv() {
                Ok(next) => {
                    counters.events_in.fetch_add(1, Ordering::Relaxed);
                    let collapses = matches!(current.kind, EventKind::Move)
                        && matches!(next.kind, EventKind::Move)
                        && current.pointer_id == next.pointer_id;
                    if collapses {
                        if let Some(older) = collapsed.take() {
                            counters.events_coalesced.fetch_add(1, Ordering::Relaxed);
                            let _ = older;
                        }
                        collapsed = Some(std::mem::replace(&mut current, next));
                    } else {
                        process(&mut injector, current, &mut primary, &counters);
                        current = next;
                        collapsed = None;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // Sender gone: process the tail, then end.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    process(&mut injector, current, &mut primary, &counters);
                    if let Some(older) = collapsed.take() {
                        counters.events_coalesced.fetch_add(1, Ordering::Relaxed);
                        let _ = older;
                    }
                    injector.release();
                    return counters;
                }
            }
        }
        process(&mut injector, current, &mut primary, &counters);
        if let Some(older) = collapsed.take() {
            counters.events_coalesced.fetch_add(1, Ordering::Relaxed);
            let _ = older;
        }
    }
    // Session over: force the injector to release (no stuck clicks).
    injector.release();
    counters
}

/// Applies the contact policy to one event and injects it when it
/// belongs to the primary contact.
fn process(
    injector: &mut Box<dyn PointerInjector>,
    event: InputEvent,
    primary: &mut Option<u8>,
    counters: &InputCounters,
) {
    match event.kind {
        EventKind::Down => {
            if primary.is_some() {
                // A second simultaneous contact is ignored: multi-touch
                // gestures are out of M1's pointer scope.
                counters.events_coalesced.fetch_add(1, Ordering::Relaxed);
                return;
            }
            *primary = Some(event.pointer_id);
            inject_one(injector.as_mut(), &event, counters);
        }
        EventKind::Move | EventKind::Hover => {
            if *primary != Some(event.pointer_id) {
                counters.events_coalesced.fetch_add(1, Ordering::Relaxed);
                return;
            }
            if event.kind == EventKind::Hover {
                // Pen hover carries no mouse meaning in M1.
                counters.events_coalesced.fetch_add(1, Ordering::Relaxed);
                return;
            }
            inject_one(injector.as_mut(), &event, counters);
        }
        EventKind::Up | EventKind::Cancel => {
            if *primary != Some(event.pointer_id) {
                counters.events_coalesced.fetch_add(1, Ordering::Relaxed);
                return;
            }
            *primary = None;
            inject_one(injector.as_mut(), &event, counters);
        }
    }
}

/// Injects one event, counting the outcome. Injection errors are
/// logged once per burst and counted; the worker keeps running so a
/// transient failure (e.g. a consent hiccup) does not kill input.
fn inject_one(injector: &mut dyn PointerInjector, event: &InputEvent, counters: &InputCounters) {
    match injector.inject(event) {
        Ok(()) => {
            counters.events_injected.fetch_add(1, Ordering::Relaxed);
        }
        Err(err) => {
            if counters.injection_errors.fetch_add(1, Ordering::Relaxed) == 0 {
                eprintln!("input: injection failed ({err}); continuing");
            }
        }
    }
}
