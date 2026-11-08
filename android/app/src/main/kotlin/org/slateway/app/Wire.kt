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

    /** Maximum ciphertext bytes per Noise record (docs/PROTOCOL.md,
     * "Noise records"). */
    const val MAX_RECORD: Int = 65535

    /** Maximum plaintext bytes per record: the Noise tag is 16 bytes. */
    const val MAX_PLAINTEXT: Int = MAX_RECORD - 16

    /** All documented flag bits; bits 2 to 7 are rejected when set. */
    const val ALL_FLAG_BITS: Int = 0b11

    const val CHANNEL_CONTROL: Int = 0
    const val HELLO_TYPE: Int = 1
    const val HELLO_ACK_TYPE: Int = 2
    const val CLOSE_TYPE: Int = 3
}

/**
 * The 6-digit pairing code from the handshake hash (docs/PROTOCOL.md,
 * "Noise records"): first 4 bytes, big-endian, mod 1,000,000,
 * zero-padded. Same derivation as the host's Rust pairing_code().
 */
fun pairingCode(handshakeHash: ByteArray): String {
    require(handshakeHash.size >= 4) { "handshake hash too short" }
    val value = ((handshakeHash[0].toLong() and 0xff) shl 24) or
        ((handshakeHash[1].toLong() and 0xff) shl 16) or
        ((handshakeHash[2].toLong() and 0xff) shl 8) or
        (handshakeHash[3].toLong() and 0xff)
    return "%06d".format(value % 1_000_000)
}

/** Reads one Noise record into `buffer`; returns its length. */
fun readRecord(input: DataInputStream, buffer: ByteArray): Int {
    val lenBytes = ByteArray(2)
    input.readFully(lenBytes)
    val len = Frame.getU16(lenBytes, 0)
    if (len == 0 || len > Protocol.MAX_RECORD) {
        throw ProtocolException("record length $len is out of range")
    }
    input.readFully(buffer, 0, len)
    return len
}

/** Writes one Noise record: `u16` little-endian length plus ciphertext. */
fun writeRecord(output: java.io.OutputStream, ciphertext: ByteArray) {
    if (ciphertext.isEmpty() || ciphertext.size > Protocol.MAX_RECORD) {
        throw ProtocolException("record length ${ciphertext.size} is out of range")
    }
    val lenBytes = ByteArray(2)
    Frame.putU16(lenBytes, 0, ciphertext.size)
    output.write(lenBytes)
    output.write(ciphertext)
}

/** One parsed 16-byte header without its payload. */
class FrameHeader(
    val channel: Int,
    val flags: Int,
    val type: Int,
    val payloadSize: Int,
    val ptsUs: Long,
) {
    /** Encodes the header, little-endian. */
    fun encode(): ByteArray {
        val out = ByteArray(Protocol.HEADER_LEN)
        out[0] = channel.toByte()
        out[1] = flags.toByte()
        Frame.putU16(out, 2, type)
        Frame.putU32(out, 4, payloadSize)
        Frame.putU64(out, 8, ptsUs)
        return out
    }

    companion object {
        /** Parses and validates a header: reserved channels (9 to 15),
         * undocumented flag bits and oversized lengths are rejected,
         * matching the Rust frame codec. */
        fun parse(bytes: ByteArray): FrameHeader {
            require(bytes.size >= Protocol.HEADER_LEN) { "header needs ${Protocol.HEADER_LEN} bytes" }
            val header = FrameHeader(
                channel = bytes[0].toInt() and 0xff,
                flags = bytes[1].toInt() and 0xff,
                type = Frame.getU16(bytes, 2),
                payloadSize = Frame.getU32(bytes, 4),
                ptsUs = Frame.getU64(bytes, 8),
            )
            if (header.channel in 9..15) {
                throw ProtocolException("reserved channel id ${header.channel}")
            }
            if (header.flags and Protocol.ALL_FLAG_BITS.inv() != 0) {
                throw ProtocolException("reserved flag bits set: ${header.flags}")
            }
            if (header.payloadSize > Protocol.MAX_PAYLOAD_LEN) {
                throw ProtocolException("payload length ${header.payloadSize} exceeds the limit")
            }
            return header
        }
    }
}

/**
 * Carries frames over the encrypted Noise transport (docs/PROTOCOL.md,
 * "Noise records"): outgoing frames are split into record-sized
 * plaintexts, incoming records are decrypted and concatenated back
 * into the frame stream. Record decrypts allocate per record on this
 * side; the media path replaces this with pooled buffers in M1.
 */
class SecureChannel(private val noise: NoiseSession) {

    /** Decrypted bytes not yet consumed by the frame parser. */
    private var pending = ByteArray(0)

    /** Encrypts and writes one frame; frames larger than one record
     * are split, the peer reassembles them. */
    fun writeFrame(output: java.io.OutputStream, header: FrameHeader, payload: ByteArray) {
        require(payload.size <= Protocol.MAX_PAYLOAD_LEN) { "payload ${payload.size} exceeds the limit" }
        val headerBytes = header.encode()
        var headerLeft = Protocol.HEADER_LEN
        var offset = 0
        do {
            val room = Protocol.MAX_PLAINTEXT - headerLeft
            val chunkLen = minOf(room, payload.size - offset)
            val plaintext = ByteArray(headerLeft + chunkLen)
            headerBytes.copyInto(plaintext, 0, 0, headerLeft)
            payload.copyInto(plaintext, headerLeft, offset, offset + chunkLen)
            writeRecord(output, noise.encrypt(plaintext))
            offset += chunkLen
            headerLeft = 0
        } while (offset < payload.size)
    }

    /** Reads one complete frame: its header and exact-size payload. */
    fun readFrame(input: DataInputStream): Pair<FrameHeader, ByteArray> {
        val recordBuf = ByteArray(Protocol.MAX_RECORD)
        while (true) {
            if (pending.size >= Protocol.HEADER_LEN) {
                val header = FrameHeader.parse(pending)
                val total = Protocol.HEADER_LEN + header.payloadSize
                while (pending.size < total) {
                    val len = readRecord(input, recordBuf)
                    pending += noise.decrypt(recordBuf.copyOf(len))
                }
                val payload = pending.copyOfRange(Protocol.HEADER_LEN, total)
                pending = pending.copyOfRange(total, pending.size)
                return Pair(header, payload)
            }
            val len = readRecord(input, recordBuf)
            pending += noise.decrypt(recordBuf.copyOf(len))
        }
    }
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
            val headerBytes = ByteArray(Protocol.HEADER_LEN)
            input.readFully(headerBytes)
            val header = FrameHeader.parse(headerBytes)
            val payload = ByteArray(header.payloadSize)
            input.readFully(payload)
            return Frame(header.channel, header.flags, header.type, payload, header.ptsUs)
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
