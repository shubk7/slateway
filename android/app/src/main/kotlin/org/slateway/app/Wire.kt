// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import slateway.control.v0.Hello
import slateway.control.v0.VideoCodec
import java.io.DataInputStream
import java.net.ProtocolException

/**
 * Wire constants (docs/PROTOCOL.md). The Kotlin and Rust sides pin the
 * same numbers; a change here is a protocol change.
 */
object Protocol {
    /** Draft v0 (docs/PROTOCOL.md title). */
    const val PROTOCOL_MAJOR: Int = 0
    const val PROTOCOL_MINOR: Int = 0
    const val DEFAULT_PORT: Int = 47820
    const val MAX_PAYLOAD_LEN: Int = 16 * 1024 * 1024
    const val HEADER_LEN: Int = 16

    /** All documented flag bits; bits 2 to 7 are rejected when set. */
    const val ALL_FLAG_BITS: Int = 0b11

    const val CHANNEL_CONTROL: Int = 0
    const val HELLO_TYPE: Int = 1
    const val HELLO_ACK_TYPE: Int = 2
    const val CLOSE_TYPE: Int = 3
}

/** One frame: a 16-byte header plus a payload (docs/PROTOCOL.md). */
class Frame(
    val channel: Int,
    val flags: Int,
    val type: Int,
    val payload: ByteArray,
    val ptsUs: Long,
) {
    /** Encodes header plus payload, little-endian. */
    fun encode(): ByteArray {
        require(payload.size <= Protocol.MAX_PAYLOAD_LEN) {
            "payload ${payload.size} exceeds the limit"
        }
        val out = ByteArray(Protocol.HEADER_LEN + payload.size)
        out[0] = channel.toByte()
        out[1] = flags.toByte()
        putU16(out, 2, type)
        putU32(out, 4, payload.size)
        putU64(out, 8, ptsUs)
        payload.copyInto(out, Protocol.HEADER_LEN)
        return out
    }

    companion object {
        /**
         * Reads one frame, validating before allocating the payload:
         * reserved channels (9 to 15), undocumented flag bits and
         * oversized lengths are rejected (docs/PROTOCOL.md, "Frame
         * header").
         */
        fun readFrom(input: DataInputStream): Frame {
            val header = ByteArray(Protocol.HEADER_LEN)
            input.readFully(header)
            val channel = header[0].toInt() and 0xff
            if (channel in 9..15) {
                throw ProtocolException("reserved channel id $channel")
            }
            val flags = header[1].toInt() and 0xff
            if (flags and Protocol.ALL_FLAG_BITS.inv() != 0) {
                throw ProtocolException("reserved flag bits set: $flags")
            }
            val type = getU16(header, 2)
            val length = getU32(header, 4)
            if (length > Protocol.MAX_PAYLOAD_LEN) {
                throw ProtocolException("payload length $length exceeds the limit")
            }
            val payload = ByteArray(length)
            input.readFully(payload)
            return Frame(channel, flags, type, payload, getU64(header, 8))
        }

        fun putU16(out: ByteArray, offset: Int, value: Int) {
            out[offset] = (value and 0xff).toByte()
            out[offset + 1] = ((value shr 8) and 0xff).toByte()
        }

        fun putU32(out: ByteArray, offset: Int, value: Int) {
            for (i in 0 until 4) {
                out[offset + i] = ((value shr (8 * i)) and 0xff).toByte()
            }
        }

        fun putU64(out: ByteArray, offset: Int, value: Long) {
            for (i in 0 until 8) {
                out[offset + i] = ((value shr (8 * i)) and 0xff).toByte()
            }
        }

        fun getU16(bytes: ByteArray, offset: Int): Int =
            (bytes[offset].toInt() and 0xff) or ((bytes[offset + 1].toInt() and 0xff) shl 8)

        fun getU32(bytes: ByteArray, offset: Int): Int {
            var value = 0
            for (i in 0 until 4) {
                value = value or ((bytes[offset + i].toInt() and 0xff) shl (8 * i))
            }
            return value
        }

        fun getU64(bytes: ByteArray, offset: Int): Long {
            var value = 0L
            for (i in 0 until 8) {
                value = value or ((bytes[offset + i].toLong() and 0xff) shl (8 * i))
            }
            return value
        }
    }
}

/**
 * Builds the tablet's Hello (docs/PROTOCOL.md, "Handshake and
 * versioning"): protocol version, app version, device model, Android
 * API, supported decoders, decode limits, screen and the capability
 * bitmask (all bits reserved in M0).
 */
fun buildHello(appVersion: String, deviceModel: String, androidApi: Int): Hello =
    Hello.newBuilder()
        .setProtocolMajor(Protocol.PROTOCOL_MAJOR)
        .setProtocolMinor(Protocol.PROTOCOL_MINOR)
        .setAppVersion(appVersion)
        .setDeviceModel(deviceModel)
        .setAndroidApi(androidApi)
        .addCodecs(VideoCodec.H264_BASELINE)
        .addCodecs(VideoCodec.H264_MAIN)
        .addCodecs(VideoCodec.H264_HIGH)
        .build()
