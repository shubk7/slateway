// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
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
    private const val WRAP_ALIAS = "slateway-noise-wrap"
    private const val KEY_FILE = "noise.key"
    private const val HOST_FILE = "known-host"
    private const val KEY_BITS = 256
    private const val NONCE_BYTES = 12
    private const val KEY_BYTES = 32

    /** The static Noise private key, generated and wrapped on first
     * run, unwrapped from the Keystore afterwards. */
    fun loadOrCreatePrivateKey(context: Context): ByteArray {
        val file = File(context.filesDir, KEY_FILE)
        if (file.exists()) {
            return unwrap(file.readBytes())
        }
        val keypair = NativeNoise.generateKeypair()
        require(keypair.size == 2 * KEY_BYTES) {
            "keypair generator returned ${keypair.size} bytes"
        }
        val private = keypair.copyOfRange(0, KEY_BYTES)
        file.writeBytes(wrap(private))
        return private
    }

    /** The paired host's static public key, or null before pairing. */
    fun knownHost(context: Context): ByteArray? {
        val file = File(context.filesDir, HOST_FILE)
        if (!file.exists()) return null
        val key = file.readBytes()
        require(key.size == KEY_BYTES) { "stored host key has ${key.size} bytes" }
        return key
    }

    /** Stores the host's public key once pairing is confirmed. */
    fun storeHost(context: Context, publicKey: ByteArray) {
        require(publicKey.size == KEY_BYTES) { "host key must be $KEY_BYTES bytes" }
        File(context.filesDir, HOST_FILE).writeBytes(publicKey)
    }

    /** Forgets the paired host; the next connection pairs again. */
    fun forgetHost(context: Context) {
        File(context.filesDir, HOST_FILE).delete()
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
