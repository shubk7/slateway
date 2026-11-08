// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import slateway.control.v0.Close
import slateway.control.v0.CloseReason
import slateway.control.v0.HelloAck
import java.io.BufferedInputStream
import java.io.DataInputStream
import java.net.InetSocketAddress
import java.net.ProtocolException
import java.net.Socket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Foreground service holding the connection to the host daemon
 * (docs/PLATFORMS.md, "Foreground services"): persistent notification,
 * connectedDevice service type. The M0 shell connects, performs the
 * Hello/HelloAck handshake and holds the session open; the daemon
 * drives the echo.
 */
class ConnectionService : Service() {

    private var socket: Socket? = null
    private var running = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val port = intent?.getIntExtra(EXTRA_PORT, Protocol.DEFAULT_PORT) ?: Protocol.DEFAULT_PORT
        startForeground(
            NOTIFICATION_ID,
            notification(statusText),
            ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
        )
        if (!running) {
            running = true
            Thread { runSession(port) }.start()
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        try {
            socket?.close()
        } catch (_: Exception) {
            // The reader thread notices the closed socket on its own.
        }
        super.onDestroy()
    }

    private fun runSession(port: Int) {
        update("connecting to host…")
        if (!NativeNoise.available) {
            update("native Noise library missing (build needs cargo-ndk, see docs/DEVELOPMENT.md)")
            stopSelf()
            return
        }
        try {
            Socket().use { s ->
                synchronized(this) { socket = s }
                s.connect(InetSocketAddress("127.0.0.1", port), CONNECT_TIMEOUT_MS)
                s.tcpNoDelay = true
                s.soTimeout = HANDSHAKE_TIMEOUT_MS
                val input = DataInputStream(BufferedInputStream(s.getInputStream()))
                val output = s.getOutputStream()

                update("handshaking")
                val privateKey = TabletKeys.loadOrCreatePrivateKey(this)
                val deviceName = android.os.Build.MODEL ?: "tablet"
                NoiseSession.initiator(privateKey).use { noise ->
                    // XX over Noise records (docs/PROTOCOL.md): -> e,
                    // <- e, ee, s, es, -> s, se. Message sizes are
                    // never assumed; records are length-driven.
                    writeRecord(output, noise.writeMessage(ByteArray(0)))
                    val record = ByteArray(Protocol.MAX_RECORD)
                    var len = readRecord(input, record)
                    noise.readMessage(record.copyOf(len))
                    writeRecord(output, noise.writeMessage(deviceName.toByteArray()))

                    val hostKey = noise.remoteStatic()
                        ?: throw ProtocolException("host static key unknown after XX")
                    val code = pairingCode(noise.handshakeHash())
                    val stored = TabletKeys.knownHost(this)
                    if (stored == null) {
                        update("pairing: confirm code $code on host and tablet")
                        requestPairing(code)
                    } else if (!stored.contentEquals(hostKey)) {
                        // A changed key for a known device is a hard
                        // failure (docs/SECURITY.md).
                        throw ProtocolException("host key changed; re-pairing required")
                    }
                    noise.intoTransport()

                    // Encrypted control frames from here on.
                    val channel = SecureChannel(noise)
                    val hello = buildHello(
                        appVersion = packageManager.getPackageInfo(packageName, 0).versionName ?: "0.1.0",
                        deviceModel = deviceName,
                        androidApi = android.os.Build.VERSION.SDK_INT,
                    )
                    val helloBytes = hello.toByteArray()
                    channel.writeFrame(
                        output,
                        FrameHeader(Protocol.CHANNEL_CONTROL, 0, Protocol.HELLO_TYPE, helloBytes.size, 0),
                        helloBytes,
                    )
                    val (ackFrame, ackPayload) = channel.readFrame(input)
                    if (ackFrame.channel != Protocol.CHANNEL_CONTROL || ackFrame.type != Protocol.HELLO_ACK_TYPE) {
                        throw ProtocolException(
                            "expected HelloAck, got channel ${ackFrame.channel} type ${ackFrame.type}"
                        )
                    }
                    val ack = HelloAck.parseFrom(ackPayload)
                    if (ack.protocolMajor != Protocol.PROTOCOL_MAJOR) {
                        val close = Close.newBuilder()
                            .setReason(CloseReason.MAJOR_MISMATCH)
                            .build()
                            .toByteArray()
                        channel.writeFrame(
                            output,
                            FrameHeader(Protocol.CHANNEL_CONTROL, 0, Protocol.CLOSE_TYPE, close.size, 0),
                            close,
                        )
                        throw ProtocolException(
                            "host protocol major ${ack.protocolMajor} does not match ${Protocol.PROTOCOL_MAJOR}"
                        )
                    }
                    if (stored == null) {
                        // Pairing was confirmed; remember the host.
                        TabletKeys.storeHost(this, hostKey)
                    }

                    s.soTimeout = 0
                    update(
                        "handshake complete" + ack.appVersion.takeIf { it.isNotEmpty() }?.let { " with $it" }.orEmpty()
                    )

                    // The M0 shell answers clock-sync pings and holds
                    // the session open; the daemon drives the echo.
                    while (true) {
                        val now = android.os.SystemClock.elapsedRealtimeNanos() / 1000
                        val (frame, payload) = channel.readFrame(input)
                        if (frame.channel == Protocol.CHANNEL_CONTROL && frame.type == Protocol.PING_TYPE) {
                            val pong = buildPong(payload, now, android.os.SystemClock.elapsedRealtimeNanos() / 1000)
                            channel.writeFrame(
                                output,
                                FrameHeader(Protocol.CHANNEL_CONTROL, 0, Protocol.PONG_TYPE, pong.size, 0),
                                pong,
                            )
                        }
                        // Other frames are discarded for now.
                    }
                }
            }
        } catch (e: Exception) {
            // Never log frame or input contents (docs/SECURITY.md): the
            // exception class and our own short messages only.
            update("disconnected: ${e.javaClass.simpleName}")
        } finally {
            synchronized(this) { socket = null }
            running = false
            stopSelf()
        }
    }

    /**
     * Blocks until the user confirms or cancels the pairing prompt in
     * the activity, or until the timeout runs out (docs/SECURITY.md:
     * the code is confirmed on the host and on the tablet).
     */
    private fun requestPairing(code: String) {
        val latch = CountDownLatch(1)
        synchronized(pairing) {
            pairing.code = code
            pairing.latch = latch
        }
        val confirmed: Boolean
        try {
            confirmed = latch.await(PAIRING_TIMEOUT_MS, TimeUnit.MILLISECONDS) && pairing.confirmed
        } catch (_: InterruptedException) {
            throw ProtocolException("pairing was interrupted")
        } finally {
            synchronized(pairing) {
                pairing.code = null
                pairing.latch = null
            }
        }
        if (!confirmed) {
            throw ProtocolException("pairing was not confirmed")
        }
    }

    private fun update(text: String) {
        statusText = text
        try {
            getSystemService(NotificationManager::class.java)
                .notify(NOTIFICATION_ID, notification(text))
        } catch (_: Exception) {
            // Status text still updates the activity; notifications may
            // be unavailable (permission denied).
        }
    }

    private fun notification(text: String): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        if (manager.getNotificationChannel(CHANNEL_ID) == null) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "Slateway connection", NotificationManager.IMPORTANCE_LOW)
            )
        }
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setSmallIcon(android.R.drawable.ic_menu_manage)
            .build()
    }

    companion object {
        const val EXTRA_PORT = "port"
        const val DEFAULT_PORT = Protocol.DEFAULT_PORT
        private const val CHANNEL_ID = "connection"
        private const val NOTIFICATION_ID = 1
        private const val CONNECT_TIMEOUT_MS = 5000
        private const val HANDSHAKE_TIMEOUT_MS = 5000
        private const val PAIRING_TIMEOUT_MS = 120_000L

        /**
         * Pairing prompt state; the activity reads `code` and calls
         * [PairingState.confirm]. Contents never include frame or
         * input data (docs/SECURITY.md).
         */
        val pairing = PairingState()

        /** Connection status for the activity. */
        @Volatile
        var statusText: String = "disconnected"
            private set
    }

    /** The pending pairing prompt, if any. */
    class PairingState {
        /** The 6-digit code while a prompt is pending, else null. */
        @Volatile
        var code: String? = null
            internal set

        @Volatile
        internal var confirmed = false

        @Volatile
        internal var latch: CountDownLatch? = null

        /** The user confirmed the code on this tablet. */
        fun confirm() {
            confirmed = true
            latch?.countDown()
        }

        /** The user dismissed the prompt. */
        fun cancel() {
            latch?.countDown()
        }
    }
}
