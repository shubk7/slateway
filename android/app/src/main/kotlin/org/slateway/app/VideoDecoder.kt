// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
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
 * The codec opens at the first keyframe, not at Configure: the AU
 * carries the parameter sets in-band (annexb), and vendor decoders
 * (Exynos among them) want them as csd-0/csd-1 at configure time —
 * the first keyframe is the earliest point both exist. After a decode
 * error the codec re-opens the same way on the next keyframe: the
 * host re-opens its encoder on the keyframe request, so the stream
 * restarts with SPS/PPS + IDR and the picture returns on its own.
 *
 * Threading: decode() runs on the connection's reader thread; the
 * SurfaceView owns the output Surface. AUs arrive one frame per
 * video-channel frame, keyframes flagged by the header.
 */
class VideoDecoder {

    /** Decode timing of the last frame, for the Stats report. */
    var lastDecodeUs: Long = 0
        private set

    /** AUs waiting for a Surface; replaced by the newest frame. */
    var queueDepth: Int = 0
        private set

    private var codec: MediaCodec? = null
    private var width: Int = 0
    private var height: Int = 0
    private var surface: Surface? = null

    /** The AVC inventory dump runs once per decoder instance: every
     * failed open repeats otherwise (the host re-opens its encoder on
     * each keyframe request). */
    private var inventoryDumped = false

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
     * (Re)configures the geometry for a new stream. Called on Configure
     * and on StreamStart. Releases any previous decoder first: the
     * stream's first AU is a keyframe, so a fresh start is safe. The
     * codec itself opens at the first keyframe (see class doc).
     */
    fun configure(width: Int, height: Int, surface: Surface?) {
        release()
        this.width = width
        this.height = height
        this.surface = surface
        if (surface == null) {
            // No display surface yet (the activity is not visible);
            // AUs queue newest-only until one shows up.
            Log.i(TAG, "decoder waiting for a surface")
        }
    }

    /**
     * Feeds one access unit. Non-keyframes before the first keyframe
     * are dropped (decoder not configured / mid-stream join); the
     * first keyframe after a configure or an error opens the codec
     * with the AU's parameter sets. AUs replace each other
     * newest-frame-wins while the codec is busy; nothing here blocks
     * the reader thread.
     */
    fun decode(data: ByteArray, keyframe: Boolean) {
        val started = android.os.SystemClock.elapsedRealtimeNanos()
        try {
            if (codec == null) {
                if (!keyframe) {
                    // Nothing to sync to; skip until the IDR.
                    return
                }
                val surface = this.surface ?: run {
                    pending = data
                    pendingKeyframe = true
                    queueDepth = 1
                    return
                }
                openCodec(surface, data)
                // The AU that fed the reopen is consumed; a later
                // attachSurface must not replay it.
                pending = null
                queueDepth = 0
            }
            feed(data, keyframe)
        } catch (e: Exception) {
            // A bad AU or a stale codec must not kill the session; the
            // tablet asks for a keyframe and the host re-opens its
            // encoder (docs/PROTOCOL.md, "Video session flow").
            lastError = describe(e)
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
        val waitingKeyframe = pendingKeyframe
        this.surface = surface
        release()
        if (waiting != null) {
            // Same path as decode(): open at the keyframe with its
            // parameter sets, then feed it.
            try {
                openCodec(surface, waiting)
                feed(waiting, waitingKeyframe)
            } catch (e: Exception) {
                lastError = describe(e)
                Log.w(TAG, "queued keyframe decode failed", e)
                release()
            }
        }
        pending = null
        queueDepth = 0
    }

    /**
     * Configures the decoder with the parameter sets found in the
     * keyframe AU (csd-0 = SPS, csd-1 = PPS, the AVC convention). AUs
     * without them still open: in-band parameter sets cover the
     * decoders that accept them.
     *
     * A configure rejection carries no readable message on some
     * devices (CodecException with an empty message), so the open
     * ladders through format variants (no low-latency key, no csd)
     * and then through every AVC decoder on the device by name — the
     * default pick of createDecoderByType need not be the one that
     * accepts the panel's resolution. If everything fails, the AVC
     * decoder inventory with per-size support is logged once so the
     * next session pinpoints the rejected key or dimension.
     */
    private fun openCodec(surface: Surface, keyframeAu: ByteArray) {
        val (sps, pps) = parameterSets(keyframeAu)
        Log.i(TAG, "decoder open ${width}x${height}: csd sps=${sps?.size ?: -1} B, pps=${pps?.size ?: -1} B")
        val variants = linkedMapOf(
            "csd+low-latency" to buildFormat(sps, pps, lowLatency = true),
            "csd" to buildFormat(sps, pps, lowLatency = false),
            "no-csd" to buildFormat(null, null, lowLatency = false),
        )
        val failures = StringBuilder()
        for ((label, format) in variants) {
            if (tryOpen(null, format, label, failures)) return
        }
        for (info in avcDecoders()) {
            for ((label, format) in variants) {
                if (tryOpen(info.name, format, "${info.name} $label", failures)) return
            }
        }
        if (!inventoryDumped) {
            inventoryDumped = true
            dumpAvcInventory()
        }
        lastError = failures.toString().trimEnd(';', ' ')
        Log.e(TAG, "decoder open failed after every variant")
    }

    private fun buildFormat(sps: ByteArray?, pps: ByteArray?, lowLatency: Boolean): MediaFormat {
        val format = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height)
        sps?.let { format.setByteBuffer("csd-0", java.nio.ByteBuffer.wrap(it)) }
        pps?.let { format.setByteBuffer("csd-1", java.nio.ByteBuffer.wrap(it)) }
        if (lowLatency && android.os.Build.VERSION.SDK_INT >= 30) {
            // Latency keys vary per codec; verify on the reference
            // devices (spike S4) before relying on them.
            format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
        }
        return format
    }

    /** Creates, configures and starts one candidate; releases it on
     * failure and records a one-line reason. `name == null` picks the
     * platform default (createDecoderByType). */
    private fun tryOpen(
        name: String?,
        format: MediaFormat,
        label: String,
        failures: StringBuilder,
    ): Boolean {
        var candidate: MediaCodec? = null
        try {
            candidate = if (name == null) {
                MediaCodec.createDecoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
            } else {
                MediaCodec.createByCodecName(name)
            }
            candidate.configure(format, surface, null, 0)
            candidate.start()
            codec = candidate
            Log.i(TAG, "decoder opened ($label)")
            return true
        } catch (e: Exception) {
            val reason = describe(e)
            Log.e(TAG, "decoder open failed ($label): $reason", e)
            failures.append("[$label] $reason; ")
            try {
                candidate?.release()
            } catch (releaseFailed: Exception) {
                Log.w(TAG, "codec release after a failed open failed", releaseFailed)
            }
            codec = null
            return false
        }
    }

    /** Every AVC decoder the device offers, in the platform's order. */
    private fun avcDecoders(): List<MediaCodecInfo> {
        return try {
            MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.filter {
                !it.isEncoder && it.supportedTypes.contains(MediaFormat.MIMETYPE_VIDEO_AVC)
            }
        } catch (e: Exception) {
            Log.w(TAG, "decoder inventory unavailable", e)
            emptyList()
        }
    }

    /** One line per AVC decoder: does it advertise the size at all? */
    private fun dumpAvcInventory() {
        for (info in avcDecoders()) {
            try {
                val caps = info.getCapabilitiesForType(MediaFormat.MIMETYPE_VIDEO_AVC)
                    .getVideoCapabilities()
                Log.w(
                    TAG,
                    "avc decoder '${info.name}': ${width}x$height supported=" +
                        "${caps.isSizeSupported(width, height)}, widths=${caps.supportedWidths}, " +
                        "heights=${caps.supportedHeights}, align=${caps.widthAlignment}x${caps.heightAlignment}",
                )
            } catch (e: Exception) {
                Log.w(TAG, "avc decoder '${info.name}': capabilities unavailable", e)
            }
        }
    }

    /** A non-empty one-line reason for the session log: CodecException
     * messages are empty on some devices; the diagnostics live in
     * getDiagnosticInfo()/getErrorCode(). */
    private fun describe(e: Exception): String {
        if (e is MediaCodec.CodecException) {
            return "CodecException code=${e.errorCode} diag=${e.diagnosticInfo}"
        }
        return e.message?.takeIf { it.isNotEmpty() } ?: e.javaClass.simpleName
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

/**
 * The annexb parameter sets of one access unit: SPS and PPS bytes
 * (start codes included) as a pair, each null when absent. Encoders
 * prepend SPS/PPS to IDR access units; P-frames carry neither.
 *
 * Annexb: NALs separated by start codes of three (`00 00 01`) or four
 * (`00 00 00 01`) bytes. A NAL's type is the low five bits of its
 * first byte; 7 is SPS, 8 is PPS. A NAL spans from its start code to
 * the next start code's first byte (the trailing zeros before a
 * four-byte start code stay with the following NAL).
 */
internal fun parameterSets(au: ByteArray): Pair<ByteArray?, ByteArray?> {
    // Start codes as (codeStart, codeLen) pairs; the payload begins
    // at codeStart + codeLen.
    val codeStarts = ArrayList<Int>(4)
    val codeLens = ArrayList<Int>(4)
    var i = 0
    while (i < au.size - 2) {
        if (au[i].toInt() == 0 && au[i + 1].toInt() == 0 && au[i + 2].toInt() == 1) {
            codeStarts.add(i)
            codeLens.add(3)
            i += 3
        } else if (
            i < au.size - 3 &&
            au[i].toInt() == 0 &&
            au[i + 1].toInt() == 0 &&
            au[i + 2].toInt() == 0 &&
            au[i + 3].toInt() == 1
        ) {
            codeStarts.add(i)
            codeLens.add(4)
            i += 4
        } else {
            i++
        }
    }
    var sps: ByteArray? = null
    var pps: ByteArray? = null
    for (j in codeStarts.indices) {
        val payloadAt = codeStarts[j] + codeLens[j]
        val type = au[payloadAt].toInt() and 0x1f
        if (type != 7 && type != 8) continue
        // Up to the next start code, or the end of the AU. Annexb
        // trailing zero bytes are ambiguous; leaving them on the final
        // NAL is safe: parsers stop at the RBSP stop bit.
        val end = if (j + 1 < codeStarts.size) codeStarts[j + 1] else au.size
        val nal = au.copyOfRange(codeStarts[j], end)
        if (type == 7) sps = nal else pps = nal
    }
    return Pair(sps, pps)
}
