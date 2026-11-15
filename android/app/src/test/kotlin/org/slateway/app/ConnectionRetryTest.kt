// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The reconnect-after-cable-pull schedule (docs/ROADMAP.md M1): pure
 * scheduling math from ConnectionService's companion, no Android
 * framework involvement.
 */
class ConnectionRetryTest {

    @Test
    fun backoffDoublesThenCaps() {
        assertEquals(1_000L, ConnectionService.retryDelayMs(1))
        assertEquals(2_000L, ConnectionService.retryDelayMs(2))
        assertEquals(4_000L, ConnectionService.retryDelayMs(3))
        assertEquals(8_000L, ConnectionService.retryDelayMs(4))
        assertEquals(8_000L, ConnectionService.retryDelayMs(5))
        assertEquals(8_000L, ConnectionService.retryDelayMs(40))
    }

    @Test
    fun attemptWindowCoversAReplug() {
        // The total wait must comfortably exceed a cable replug's
        // reach (seconds) without growing without bound when the
        // host stays away.
        val total = (1..ConnectionService.MAX_RETRY_ATTEMPTS)
            .sumOf { ConnectionService.retryDelayMs(it) }
        assertTrue(
            "total retry window ${total / 1000} s should be minutes, not hours",
            total > 120_000L && total < 15 * 60_000L,
        )
    }

    @Test
    fun firstAttemptIsImmediate() {
        // attempt 1 connects right away; the delay only applies to
        // the second and later tries.
        assertTrue(ConnectionService.retryDelayMs(1) >= 500L)
    }
}
