// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * The annexb parameter-set extractor the decoder configures csd-0/csd-1
 * from. Shapes reflect what the host's encoders emit (verified against
 * an h264_nvenc dump: SPS, PPS, SEI, IDR in one annexb AU).
 */
class ParameterSetsTest {

    private fun startCode3() = byteArrayOf(0, 0, 1)
    private fun startCode4() = byteArrayOf(0, 0, 0, 1)

    private fun nal(type: Int, body: Byte) = byteArrayOf((type or 0x60).toByte(), body)

    @Test
    fun sps_and_pps_extracted_from_three_byte_codes() {
        val sps = startCode3() + nal(7, 1)
        val pps = startCode3() + nal(8, 2)
        val idr = startCode3() + nal(5, 3)
        val (gotSps, gotPps) = parameterSets(sps + pps + idr)
        assertArrayEquals(sps, gotSps)
        assertArrayEquals(pps, gotPps)
    }

    @Test
    fun four_byte_start_codes_extracted() {
        val sps = startCode4() + nal(7, 1)
        val pps = startCode4() + nal(8, 2)
        val (gotSps, gotPps) = parameterSets(sps + pps)
        assertArrayEquals(sps, gotSps)
        assertArrayEquals(pps, gotPps)
    }

    /** nvenc's IDR order: SPS, PPS, SEI (type 6), then the IDR NAL. */
    @Test
    fun sei_and_idr_skipped() {
        val sps = startCode3() + nal(7, 1)
        val pps = startCode3() + nal(8, 2)
        val sei = startCode3() + nal(6, 4) + startCode3() + nal(6, 5)
        val idr = startCode3() + nal(5, 3)
        val (gotSps, gotPps) = parameterSets(sps + pps + sei + idr)
        assertArrayEquals(sps, gotSps)
        assertArrayEquals(pps, gotPps)
    }

    @Test
    fun p_frame_has_no_parameter_sets() {
        val p = startCode3() + nal(1, 9)
        val (sps, pps) = parameterSets(p)
        assertNull(sps)
        assertNull(pps)
    }

    @Test
    fun absent_start_codes_yield_nulls() {
        val (sps, pps) = parameterSets(byteArrayOf(1, 2, 3, 4))
        assertNull(sps)
        assertNull(pps)
    }

    /** A four-byte start code's leading zero is not part of the NAL. */
    @Test
    fun leading_zero_of_next_start_code_stays_with_next_nal() {
        val sps = startCode3() + nal(7, 1)
        val pps = startCode4() + nal(8, 2)
        val (gotSps, gotPps) = parameterSets(sps + pps)
        // The SPS slice ends before the four-byte code's first zero.
        assertArrayEquals(sps, gotSps)
        assertArrayEquals(pps, gotPps)
    }

    /** The extractor must not read past the AU's end. */
    @Test
    fun truncated_start_code_at_end_ignored() {
        val (sps, pps) = parameterSets(startCode3() + nal(7, 1) + byteArrayOf(0, 0))
        assertNull(pps)
        // The unrecognizable trailing zeros stay with the final NAL
        // (parsers stop at the RBSP stop bit).
        assertEquals(7, sps?.size)
    }
}
