// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

/**
 * JNI declarations over the Rust Noise facade (android/native),
 * ADR-013: the same snow implementation the host runs. The facade
 * holds no I/O; this side owns records and frames (docs/PROTOCOL.md).
 */
object NativeNoise {
    /** False when the APK was built without the NDK toolchain. */
    val available: Boolean by lazy {
        try {
            System.loadLibrary("slateway_native")
            true
        } catch (_: UnsatisfiedLinkError) {
            false
        }
    }

    external fun generateKeypair(): ByteArray

    external fun newInitiator(privateKey: ByteArray): Long

    external fun newResponder(privateKey: ByteArray): Long

    external fun writeMessage(ptr: Long, payload: ByteArray): ByteArray

    external fun readMessage(ptr: Long, message: ByteArray): ByteArray

    external fun handshakeHash(ptr: Long): ByteArray

    external fun remoteStatic(ptr: Long): ByteArray?

    external fun intoTransport(ptr: Long): Long

    external fun encrypt(ptr: Long, plaintext: ByteArray): ByteArray

    external fun decrypt(ptr: Long, ciphertext: ByteArray): ByteArray

    external fun free(ptr: Long)
}

/**
 * Owns one native Noise session; close exactly once. Methods throw
 * IllegalStateException from the native side on protocol errors.
 */
class NoiseSession private constructor(@Volatile private var ptr: Long) : AutoCloseable {

    private fun requireOpen(): Long {
        val current = ptr
        require(current != 0L) { "session is closed" }
        return current
    }

    /** Handshake write; throws once the handshake is over. */
    fun writeMessage(payload: ByteArray): ByteArray = NativeNoise.writeMessage(requireOpen(), payload)

    /** Handshake read; throws once the handshake is over. */
    fun readMessage(message: ByteArray): ByteArray = NativeNoise.readMessage(requireOpen(), message)

    fun handshakeHash(): ByteArray = NativeNoise.handshakeHash(requireOpen())

    /** The remote static public key, or null before it is known. */
    fun remoteStatic(): ByteArray? = NativeNoise.remoteStatic(requireOpen())

    /** Finishes the handshake; the session switches to encrypted
     * transport. */
    fun intoTransport() {
        ptr = NativeNoise.intoTransport(requireOpen())
    }

    fun encrypt(plaintext: ByteArray): ByteArray = NativeNoise.encrypt(requireOpen(), plaintext)

    fun decrypt(ciphertext: ByteArray): ByteArray = NativeNoise.decrypt(requireOpen(), ciphertext)

    override fun close() {
        val current = ptr
        if (current != 0L) {
            ptr = 0
            NativeNoise.free(current)
        }
    }

    companion object {
        /** New initiator (the tablet's role). */
        fun initiator(privateKey: ByteArray): NoiseSession =
            NoiseSession(NativeNoise.newInitiator(privateKey))

        /** New responder (the host's role). */
        fun responder(privateKey: ByteArray): NoiseSession =
            NoiseSession(NativeNoise.newResponder(privateKey))
    }
}
