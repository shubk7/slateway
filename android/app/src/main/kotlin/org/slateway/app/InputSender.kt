// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import android.view.MotionEvent
import android.view.View
import java.io.DataOutputStream
import java.util.concurrent.LinkedBlockingDeque
import java.util.concurrent.TimeUnit

/**
 * Tablet touch → host pointer (docs/ROADMAP.md M1, "Touch as
 * pointer"; docs/PROTOCOL.md, "Input event"). Runs only when the
 * host's HelloAck announced [Protocol.CAPABILITY_INPUT]: the
 * operator opted in on the daemon (`--allow-input`).
 *
 * Touches arrive on the UI thread, which extracts plain samples
 * (MotionEvents are recycled) into a bounded queue; a sender thread
 * coalesces move runs (docs/PROTOCOL.md, "Backpressure": input is
 * never dropped, moves coalesce) and writes one batch per wakeup
 * under the session's write lock — the same lock the clock-sync
 * Pongs use, since the Noise session is stateful.
 *
 * Coordinates are normalized 0..65535 across the streamed display;
 * the view is the streamed display (the decoder scales the video to
 * it). M1 maps every pointer's events with its pointerId — the host
 * picks the primary contact; extra contacts are ignored there.
 */
class InputSender(
    private val channel: SecureChannel,
    private val output: DataOutputStream,
    private val writeLock: Any,
) {
    /** One extracted touch sample, ready to pack. */
    data class Sample(
        val kind: Int, // EventKind wire id: 0 down, 1 move, 2 up
        val tool: Int, // 0 finger, 1 pen, 2 eraser
        val pointerId: Int,
        val x: Int, // 0..65535
        val y: Int, // 0..65535
        val pressure: Int, // 0..65535, 0 for fingers
        val ptsUs: Long,
    )

    private val queue = LinkedBlockingDeque<Sample>(QUEUE_CAPACITY)
    @Volatile
    private var stopped = false
    private val thread = Thread({ run() }, "slateway-input")

    /** Starts the sender thread. */
    fun start() {
        thread.start()
    }

    /** Stops the sender (session teardown); pending samples drop. */
    fun stop() {
        stopped = true
        thread.interrupt()
    }

    /** Extracts a MotionEvent into samples and queues them. Called
     * on the UI thread; extraction is cheap, the socket write is not
     * done here. Returns true when the event was consumed. */
    fun onTouch(view: View, event: MotionEvent): Boolean {
        if (stopped) return false
        if (view.width <= 0 || view.height <= 0) return false
        val ptsUs = event.eventTime * 1000L
        val samples = ArrayList<Sample>(event.pointerCount)
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> addPointer(view, event, 0, KIND_DOWN, ptsUs, samples)
            MotionEvent.ACTION_POINTER_DOWN ->
                addPointer(view, event, event.actionIndex, KIND_DOWN, ptsUs, samples)
            MotionEvent.ACTION_MOVE -> {
                for (i in 0 until event.pointerCount) {
                    addPointer(view, event, i, KIND_MOVE, ptsUs, samples)
                }
            }
            MotionEvent.ACTION_POINTER_UP ->
                addPointer(view, event, event.actionIndex, KIND_UP, ptsUs, samples)
            MotionEvent.ACTION_UP -> addPointer(view, event, 0, KIND_UP, ptsUs, samples)
            MotionEvent.ACTION_CANCEL -> {
                for (i in 0 until event.pointerCount) {
                    addPointer(view, event, i, KIND_UP, ptsUs, samples)
                }
            }
            else -> return false // hover and multi-window actions: not M1
        }
        for (sample in samples) {
            // offerFirst + drop-oldest on overflow: the newest
            // position wins (the host coalesces moves anyway).
            while (!queue.offerFirst(sample)) {
                queue.pollLast() ?: break
            }
        }
        return true
    }

    private fun addPointer(
        view: View,
        event: MotionEvent,
        index: Int,
        kind: Int,
        ptsUs: Long,
        out: MutableList<Sample>,
    ) {
        val tool = when (event.getToolType(index)) {
            MotionEvent.TOOL_TYPE_STYLUS -> TOOL_PEN
            MotionEvent.TOOL_TYPE_ERASER -> TOOL_ERASER
            else -> TOOL_FINGER
        }
        val pressure = if (tool == TOOL_FINGER) {
            0 // fingers carry no pressure (docs/PROTOCOL.md)
        } else {
            (event.pressure.coerceIn(0f, 1f) * 65535f).toInt()
        }
        out.add(
            Sample(
                kind = kind,
                tool = tool,
                pointerId = event.getPointerId(index),
                x = (event.getX(index) / view.width * 65535f).toInt().coerceIn(0, 65535),
                y = (event.getY(index) / view.height * 65535f).toInt().coerceIn(0, 65535),
                pressure = pressure,
                ptsUs = ptsUs,
            ),
        )
    }

    /** The sender loop: batch wakeups, coalesce move runs, write one
     * input frame per wakeup. */
    private fun run() {
        val batch = ArrayList<Sample>(MAX_BATCH)
        while (!stopped) {
            val first = try {
                queue.pollFirst(200, TimeUnit.MILLISECONDS) ?: continue
            } catch (_: InterruptedException) {
                return
            }
            batch.clear()
            batch.add(first)
            // Drain what is already queued (no waiting): one batch per
            // wakeup keeps the latency at the producer's cadence.
            while (batch.size < MAX_BATCH) {
                queue.pollFirst()?.let { batch.add(it) } ?: break
            }
            coalesceMoves(batch)
            if (batch.isEmpty()) continue
            try {
                writeBatch(batch)
            } catch (e: Exception) {
                // The session's reader thread owns disconnect
                // handling; stop quietly.
                return
            }
        }
    }

    /** Collapses runs of same-pointer moves to the newest sample
     * (docs/PROTOCOL.md, "Backpressure"); downs and ups always stay. */
    private fun coalesceMoves(batch: MutableList<Sample>) {
        var i = 0
        while (i < batch.size) {
            if (batch[i].kind == KIND_MOVE) {
                val pointer = batch[i].pointerId
                var j = i + 1
                while (j < batch.size && batch[j].kind == KIND_MOVE && batch[j].pointerId == pointer) {
                    j++
                }
                if (j > i + 1) {
                    // Keep the newest move of the run.
                    val newest = batch[j - 1]
                    batch.subList(i, j).clear()
                    batch.add(i, newest)
                }
                i++
            } else {
                i++
            }
        }
    }

    /** Packs the samples back to back and writes one frame under the
     * session's write lock. */
    private fun writeBatch(batch: List<Sample>) {
        val payload = ByteArray(batch.size * EVENT_LEN_BYTES)
        batch.forEachIndexed { index, sample ->
            encodeSample(sample, payload, index * EVENT_LEN_BYTES)
        }
        val header = FrameHeader(
            channel = Protocol.CHANNEL_INPUT,
            flags = 0,
            type = Protocol.INPUT_BATCH_TYPE,
            payloadSize = payload.size,
            ptsUs = batch.last().ptsUs,
        )
        synchronized(writeLock) {
            channel.writeFrame(output, header, payload)
        }
    }

    companion object {
        /** EventKind wire ids (docs/PROTOCOL.md, "Input event"). */
        const val KIND_DOWN = 0
        const val KIND_MOVE = 1
        const val KIND_UP = 2

        /** Packs one sample into the wire layout (docs/PROTOCOL.md,
         * "Input event"; byte-for-byte the Rust golden_layout test).
         * Internal for the WireTest golden-bytes check. */
        internal fun encodeSample(sample: Sample, out: ByteArray, offset: Int) {
            out[offset] = sample.kind.toByte()
            out[offset + 1] = sample.tool.toByte()
            out[offset + 2] = 0 // buttons: barrel buttons arrive in M3
            out[offset + 3] = sample.pointerId.toByte()
            Frame.putU16(out, offset + 4, sample.x)
            Frame.putU16(out, offset + 6, sample.y)
            Frame.putU16(out, offset + 8, sample.pressure)
            out[offset + 10] = 0 // tilt_x: pen work (M3)
            out[offset + 11] = 0 // tilt_y
            Frame.putU32(out, offset + 12, 0) // dt_us: pts rides the header
        }

        /** Tool wire ids. */
        const val TOOL_FINGER = 0
        const val TOOL_PEN = 1
        const val TOOL_ERASER = 2

        /** One packed event's size (docs/PROTOCOL.md). */
        const val EVENT_LEN_BYTES = 16

        /** Bounded queue; overflow drops the oldest samples (newest
         * win), matching the host's coalescing expectations. */
        private const val QUEUE_CAPACITY = 256

        /** Upper bound on one batch's event count. */
        private const val MAX_BATCH = 32
    }
}
