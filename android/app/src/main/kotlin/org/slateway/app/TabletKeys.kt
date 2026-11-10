// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Log
import java.io.File
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Tablet key storage (docs/SECURITY.md, "Pairing"): the Noise static
 * private key is generated once and wrapped by an Android Keystore
 * AES-GCM key; only the wrapped blob lives in app-private storage.
 * The host's public key is stored unwrapped after pairing (it is
 * public data). Not unit-testable on the JVM; verified on device.
 */
object TabletKeys {
    private const val TAG = "slateway"
    private const val WRAP_ALIAS = "slateway-noise-wrap"
    private const val KEY_FILE = "noise.key"
    private const val HOST_FILE = "known-hosts"
    private const val KEY_BITS = 256
    private const val NONCE_BYTES = 12
    private const val KEY_BYTES = 32

    /** The static Noise private key, generated and wrapped on first
     * run, unwrapped from the Keystore afterwards. */
    fun loadOrCreatePrivateKey(context: Context): ByteArray {
        val file = File(context.filesDir, KEY_FILE)
        if (file.exists()) {
            Log.i(TAG, "keys: noise key loaded from storage")
            return unwrap(file.readBytes())
        }
        Log.i(TAG, "keys: first run, generating noise key")
        val keypair = NativeNoise.generateKeypair()
        require(keypair.size == 2 * KEY_BYTES) {
            "keypair generator returned ${keypair.size} bytes"
        }
        val private = keypair.copyOfRange(0, KEY_BYTES)
        file.writeBytes(wrap(private))
        return private
    }

    /** Whether this host's static public key is in the trusted set.
     * Unknown hosts simply restart pairing; there is no single-host
     * pin, so a tablet moves between hosts freely. A missing or
     * unreadable file means "no trusted hosts", never an error: the
     * first run after an install must reach pairing, not crash. */
    fun isTrustedHost(context: Context, publicKey: ByteArray): Boolean {
        require(publicKey.size == KEY_BYTES) { "host key must be $KEY_BYTES bytes" }
        val file = hostKeysFile(context)
        if (!file.exists()) {
            Log.i(TAG, "keys: no trusted hosts stored yet (fresh install or forgotten)")
            return false
        }
        val lines = try {
            file.readLines()
        } catch (e: Exception) {
            Log.e(TAG, "keys: could not read known-hosts; treating as untrusted", e)
            return false
        }
        val trusted = lines.any { line ->
            hexToBytes(line.trim())?.contentEquals(publicKey) == true
        }
        Log.i(TAG, "keys: host is ${if (trusted) "trusted" else "unknown"} among ${lines.size} known")
        return trusted
    }

    /** Adds a host's public key to the trusted set once pairing is
     * confirmed; duplicates are ignored. */
    fun trustHost(context: Context, publicKey: ByteArray) {
        require(publicKey.size == KEY_BYTES) { "host key must be $KEY_BYTES bytes" }
        if (isTrustedHost(context, publicKey)) return
        hostKeysFile(context).appendText(hexEncode(publicKey) + "\n")
        Log.i(TAG, "keys: host trusted and stored")
    }

    /** Forgets every trusted host; the next connection pairs again. */
    fun forgetHosts(context: Context) {
        hostKeysFile(context).delete()
    }

    private fun hostKeysFile(context: Context): File = File(context.filesDir, HOST_FILE)

    private fun hexToBytes(text: String): ByteArray? {
        if (text.length != 2 * KEY_BYTES) return null
        return try {
            val out = ByteArray(KEY_BYTES)
            for (i in 0 until KEY_BYTES) {
                out[i] = ((Character.digit(text[2 * i], 16) shl 4)
                    + Character.digit(text[2 * i + 1], 16)).toByte()
            }
            out
        } catch (_: Exception) {
            null
        }
    }

    private fun hexEncode(bytes: ByteArray): String = buildString {
        for (byte in bytes) append("%02x".format(byte))
    }

    private fun wrapKey(): SecretKey {
        val keyStore = java.security.KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (keyStore.getKey(WRAP_ALIAS, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(
            KeyGenParameterSpec.Builder(
                WRAP_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(KEY_BITS)
                .build(),
        )
        return generator.generateKey()
    }

    /** Layout: 12-byte GCM nonce, then ciphertext plus tag. */
    private fun wrap(privateKey: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, wrapKey())
        val nonce = cipher.iv
        require(nonce.size == NONCE_BYTES) { "unexpected GCM nonce length ${nonce.size}" }
        return nonce + cipher.doFinal(privateKey)
    }

    private fun unwrap(blob: ByteArray): ByteArray {
        require(blob.size > NONCE_BYTES) { "wrapped key blob too short" }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, wrapKey(), GCMParameterSpec(128, blob, 0, NONCE_BYTES))
        return cipher.doFinal(blob, NONCE_BYTES, blob.size - NONCE_BYTES)
    }
}
