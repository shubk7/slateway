// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import android.media.MediaCodec
import android.media.MediaFormat
import android.util.Log
import android.view.Surface

/**
 * H.264 decoder over MediaCodec (docs/ROADMAP.md M1 "Video channel";
 * docs/PROTOCOL.md, "Video session flow"): newest-frame-wins so a slow
 * decode never builds latency (docs/ARCHITECTURE.md, latency rule 1),
 * low-latency keys where the device supports them (spike S4 verifies
 * the behaviour on Tab A7 and S6 Lite).
 *
 * Threading: decode() runs on the connection's reader thread; the
 * SurfaceView owns the output Surface. AUs arrive one frame per
 * video-channel frame, keyframes flagged by the header.
 */
class VideoDecoder {

    /** Decode timing of the last frame, for the Stats report. */
    var lastDecodeUs: Long = 0
        private set

    /** Access units queued and not yet decoded. */
    var queueDepth: Int = 0
        private set

    private var codec: MediaCodec? = null
    private var width: Int = 0
    private var height: Int = 0

    /** AUs waiting for a Surface; replaced by the newest frame. */
    private var pending: ByteArray? = null
    private var pendingKeyframe = false

    /** Latest decoder error for the session log; the stream resyncs on
     * RequestKeyframe. */
    var lastError: String? = null
        private set

    /** Clears the latched error after the session acted on it. */
    fun clearError() {
        lastError = null
    }

    /**
     * (Re)configures the decoder for a new stream. Called on Configure
     * and on StreamStart. Releases any previous decoder first: the
     * stream's first AU is a keyframe, so a fresh start is safe.
     */
    fun configure(width: Int, height: Int, surface: Surface?) {
        release()
        this.width = width
        this.height = height
        if (surface == null) {
            // No display surface yet (the activity is not visible);
            // AUs queue newest-only until one shows up.
            Log.i(TAG, "decoder waiting for a surface")
            return
        }
        openCodec(surface)
    }

    /**
     * Feeds one access unit. Non-keyframes before the first keyframe
     * are dropped (decoder not configured / mid-stream join). AUs
     * replace each other newest-frame-wins while the codec is not
     * ready or busy; nothing here blocks the reader thread.
     */
    fun decode(data: ByteArray, keyframe: Boolean) {
        val started = android.os.SystemClock.elapsedRealtimeNanos()
        if (codec == null) {
            if (!keyframe) {
                // Nothing to sync to; skip until the IDR.
                return
            }
            pending = data
            pendingKeyframe = true
            queueDepth = 1
            return
        }
        try {
            feed(data, keyframe)
        } catch (e: Exception) {
            // A bad AU or a stale codec must not kill the session; the
            // tablet asks for a keyframe and the host re-opens its
            // encoder (docs/PROTOCOL.md, "Video session flow").
            lastError = e.message ?: e.javaClass.simpleName
            Log.w(TAG, "decode failed, requesting keyframe", e)
            release()
            pending = data
            pendingKeyframe = keyframe
        } finally {
            lastDecodeUs = (android.os.SystemClock.elapsedRealtimeNanos() - started) / 1000
        }
    }

    /** Drops all pending AUs; called on StreamStop. */
    fun stop() {
        release()
        pending = null
        queueDepth = 0
    }

    /** Releases the codec; the Surface stays owned by the caller. */
    fun release() {
        codec?.let {
            try {
                it.stop()
            } catch (_: IllegalStateException) {}
            it.release()
        }
        codec = null
    }

    /** The pending keyframe (if any) once a Surface exists. */
    fun attachSurface(surface: Surface) {
        val waiting = pending
        release()
        openCodec(surface)
        if (waiting != null && codec != null) {
            try {
                feed(waiting, pendingKeyframe)
            } catch (e: Exception) {
                lastError = e.message ?: e.javaClass.simpleName
                Log.w(TAG, "queued keyframe decode failed", e)
            }
        }
        pending = null
        queueDepth = 0
    }

    private fun openCodec(surface: Surface) {
        val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            // Latency keys vary per codec; verify on the reference
            // devices (spike S4) before relying on them.
            format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
        }
        try {
            val newCodec = MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
            newCodec.configure(format, surface, null, 0)
            newCodec.start()
            codec = newCodec
        } catch (e: Exception) {
            lastError = e.message ?: e.javaClass.simpleName
            Log.e(TAG, "MediaCodec configure failed", e)
            codec = null
        }
    }

    /**
     * Queues one AU and drains whatever is ready. Input buffers are
     * copied per queueInputBuffer contract; output is rendered
     * directly to the Surface with no timestamp math (presentation
     * order is the AU order, the newest frame wins on the screen).
     */
    private fun feed(data: ByteArray, keyframe: Boolean) {
        val c = codec ?: return
        val info = MediaCodec.BufferInfo()
        var queued = false
        while (true) {
            val inIndex = c.dequeueInputBuffer(0L)
            if (inIndex < 0) break
            val inBuf = c.getInputBuffer(inIndex) ?: break
            val size = minOf(data.size, inBuf.capacity())
            if (size < data.size) {
                throw IllegalStateException("input buffer ${inBuf.capacity()} < AU ${data.size}")
            }
            inBuf.clear()
            inBuf.put(data)
            val flags = if (keyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
            c.queueInputBuffer(inIndex, 0, data.size, 0L, flags)
            queued = true
            break
        }
        if (!queued) {
            // All input buffers busy: keep only the newest AU.
            pending = data
            pendingKeyframe = keyframe
            queueDepth = 1
        } else {
            queueDepth = 0
        }
        while (true) {
            val outIndex = c.dequeueOutputBuffer(info, 0L)
            if (outIndex >= 0) {
                c.releaseOutputBuffer(outIndex, true)
                continue
            }
            break
        }
    }

    private companion object {
        const val TAG = "slateway-video"
    }
}
