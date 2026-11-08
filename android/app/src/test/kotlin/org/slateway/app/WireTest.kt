// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.net.ProtocolException

/**
 * Wire tests for the M0 shell. The Hello golden bytes are the same
 * array pinned by the Rust side (crates/slateway-proto
 * src/control.rs), so a schema change that passes here but not there
 * is a cross-language break, and vice versa.
 */
class WireTest {

    @Test
    fun helloGoldenWireBytes() {
        val hello = buildHello(appVersion = "0.1.0", deviceModel = "Tab", androidApi = 30)
        assertArrayEquals(
            byteArrayOf(
                // app_version, field 3: "0.1.0"
                0x1a, 0x05, 0x30, 0x2e, 0x31, 0x2e, 0x30,
                // device_model, field 4: "Tab"
                0x22, 0x03, 0x54, 0x61, 0x62,
                // android_api, field 5, varint 30
                0x28, 0x1e,
                // codecs, field 6, packed [baseline, main, high]
                0x32, 0x03, 0x01, 0x02, 0x03,
            ),
            hello.toByteArray(),
        )
    }

    @Test
    fun frameHeaderGoldenBytes() {
        val encoded = Frame(Protocol.CHANNEL_CONTROL, 0, Protocol.HELLO_TYPE, byteArrayOf(1, 2), 0x0102030405060708L).encode()
        assertArrayEquals(
            byteArrayOf(
                0x00, // channel: control
                0x00, // flags
                0x01, 0x00, // type: Hello
                0x02, 0x00, 0x00, 0x00, // payload length
                0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, // pts_us
                0x01, 0x02, // payload
            ),
            encoded,
        )
    }

    @Test
    fun frameRoundTrip() {
        val frame = Frame(Protocol.CHANNEL_CONTROL, 0b01, Protocol.CLOSE_TYPE, byteArrayOf(9, 8, 7), 0xdead_beefL)
        val parsed = Frame.readFrom(DataInputStream(ByteArrayInputStream(frame.encode())))
        assertEquals(frame.channel, parsed.channel)
        assertEquals(frame.flags, parsed.flags)
        assertEquals(frame.type, parsed.type)
        assertArrayEquals(frame.payload, parsed.payload)
        assertEquals(frame.ptsUs, parsed.ptsUs)
    }

    @Test
    fun rejectsReservedChannel() {
        val bytes = ByteArray(Protocol.HEADER_LEN)
        bytes[0] = 9
        try {
            Frame.readFrom(DataInputStream(ByteArrayInputStream(bytes)))
            fail("reserved channel must be rejected")
        } catch (expected: ProtocolException) {
            // expected
        }
    }

    @Test
    fun rejectsReservedFlagBits() {
        val bytes = ByteArray(Protocol.HEADER_LEN)
        bytes[0] = Protocol.CHANNEL_CONTROL.toByte()
        bytes[1] = 0b100
        try {
            Frame.readFrom(DataInputStream(ByteArrayInputStream(bytes)))
            fail("reserved flag bits must be rejected")
        } catch (expected: ProtocolException) {
            // expected
        }
    }

    @Test
    fun rejectsOversizedLengthBeforeAllocating() {
        val bytes = ByteArray(Protocol.HEADER_LEN)
        bytes[0] = Protocol.CHANNEL_CONTROL.toByte()
        Frame.putU32(bytes, 4, Protocol.MAX_PAYLOAD_LEN + 1)
        try {
            Frame.readFrom(DataInputStream(ByteArrayInputStream(bytes)))
            fail("oversized length must be rejected")
        } catch (expected: ProtocolException) {
            // expected
        }
    }

    @Test
    fun truncatedHeaderIsAnError() {
        try {
            Frame.readFrom(DataInputStream(ByteArrayInputStream(ByteArray(Protocol.HEADER_LEN - 1))))
            fail("truncated header must be rejected")
        } catch (expected: java.io.EOFException) {
            // expected
        }
    }

    @Test
    fun pairingCodeMatchesTheHostDerivation() {
        // Same vector as the Rust pairing_code test
        // (crates/slateway-core src/crypto.rs): 0x12345678 mod 1,000,000.
        val hash = byteArrayOf(0x12, 0x34, 0x56, 0x78, 0x7f.toByte())
        assertEquals("419896", pairingCode(hash))
    }

    @Test
    fun pairingCodeIsSixDigits() {
        for (prefix in listOf(byteArrayOf(0, 0, 0, 0), byteArrayOf(0xff.toByte(), 0xff.toByte(), 0xff.toByte(), 0xff.toByte()))) {
            val code = pairingCode(prefix)
            assertEquals(6, code.length)
            assertTrue(code.all { it.isDigit() })
        }
        assertProtocolError { pairingCode(byteArrayOf(1, 2, 3)) }
    }

    @Test
    fun recordsRoundTrip() {
        val ciphertext = ByteArray(257) { (it % 251).toByte() }
        val out = ByteArrayOutputStream()
        writeRecord(out, ciphertext)
        val read = ByteArray(Protocol.MAX_RECORD)
        val len = readRecord(DataInputStream(ByteArrayInputStream(out.toByteArray())), read)
        assertEquals(257, len)
        assertArrayEquals(ciphertext, read.copyOf(len))
    }

    @Test
    fun recordsRejectBadLengths() {
        val out = ByteArrayOutputStream()
        try {
            writeRecord(out, ByteArray(0))
            fail("empty records must be rejected")
        } catch (expected: ProtocolException) {
            // expected
        }
        try {
            writeRecord(out, ByteArray(Protocol.MAX_RECORD + 1))
            fail("oversized records must be rejected")
        } catch (expected: ProtocolException) {
            // expected
        }
        // A zero length on the wire is rejected before reading payload.
        try {
            readRecord(DataInputStream(ByteArrayInputStream(byteArrayOf(0, 0))), ByteArray(16))
            fail("zero-length records must be rejected")
        } catch (expected: ProtocolException) {
            // expected
        }
    }

    @Test
    fun frameHeaderRejectsBadFields() {
        val bytes = ByteArray(Protocol.HEADER_LEN)
        bytes[0] = 9 // reserved channel
        assertProtocolError { FrameHeader.parse(bytes) }
        bytes[0] = Protocol.CHANNEL_CONTROL.toByte()
        bytes[1] = 0b100 // undocumented flag bit
        assertProtocolError { FrameHeader.parse(bytes) }
        bytes[1] = 0
        Frame.putU32(bytes, 4, Protocol.MAX_PAYLOAD_LEN + 1)
        assertProtocolError { FrameHeader.parse(bytes) }
    }

    private fun assertProtocolError(block: () -> Unit) {
        try {
            block()
            fail("expected a failure")
        } catch (_: ProtocolException) {
            // expected
        } catch (_: IllegalArgumentException) {
            // expected
        }
    }
}
