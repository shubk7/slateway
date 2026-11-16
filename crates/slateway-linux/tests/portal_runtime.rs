// SPDX-License-Identifier: GPL-3.0-or-later

//! Regression test for the portal connection's runtime binding
//! (Linux only; skipped when no session bus is reachable, which is
//! the case on CI runners).
//!
//! Property reads are served from the proxy's cache and would pass
//! even on a dead connection, so the probe is a real method round
//! trip (D-Bus Peer.Ping) on a connection built the way the capture
//! dance builds one.

#![cfg(target_os = "linux")]

use ashpd::zbus::Connection;
use std::time::Duration;

/// How long to wait on a call to a connection whose runtime is gone
/// before concluding it is dead.
const DEAD_CALL_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn portal_connection_is_bound_to_its_build_runtime() {
    if std::env::var("DBUS_SESSION_BUS_ADDRESS").is_err() {
        // No session bus (CI runners): nothing to bind to.
        return;
    }
    // The dance runtime, exactly as PortalCapture::start builds it.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("dance runtime");
    let connection = runtime
        .block_on(async { Connection::session().await })
        .expect("session bus connection");

    // A real round trip on the build runtime works: this is what the
    // dance itself needs, and what the kept-alive runtime must keep
    // guaranteeing for the input worker's notify calls.
    async fn ping(conn: &Connection) -> Result<ashpd::zbus::Message, ashpd::zbus::Error> {
        conn.call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.DBus.Peer"),
            "Ping",
            &(),
        )
        .await
    }
    runtime
        .block_on(ping(&connection))
        .expect("call on the build runtime");

    // Dropping the build runtime kills the connection (zbus binds it
    // to the runtime it was built on; its socket reader task dies
    // with it). Seen on hardware: the first touch after setup failed
    // with a zbus "i/o error" when the dance runtime was dropped at
    // the end of PortalCapture::start. The fix keeps the runtime
    // alive with the input handle; a regression here fails calls or
    // hangs them.
    drop(runtime);
    let other = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("second runtime");
    let dead =
        other.block_on(async { tokio::time::timeout(DEAD_CALL_TIMEOUT, ping(&connection)).await });
    assert!(
        !matches!(dead, Ok(Ok(_))),
        "the portal connection outlived the runtime it was built on; \
         the binding assumption changed, re-check PortalCapture::start"
    );
}
