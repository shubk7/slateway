// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import android.util.Log
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
        if (android.os.Build.VERSION.SDK_INT >= 34) {
            startForeground(
                NOTIFICATION_ID,
                notification(statusText),
                ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
            )
        } else {
            // Types are not enforced below Android 14; the two-argument
            // call avoids passing a constant the platform does not know.
            startForeground(NOTIFICATION_ID, notification(statusText))
        }
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
        val tag = "slateway"
        // The step the session thread is in; shown on disconnect so a
        // failure says WHERE it happened, not just the exception class.
        var stage = "starting"
        update("connecting to host…")
        if (!NativeNoise.available) {
            update("native Noise library missing (build needs cargo-ndk, see docs/DEVELOPMENT.md)")
            stopSelf()
            return
        }
        try {
            Socket().use { s ->
                synchronized(this) { socket = s }
                stage = "connecting to host"
                s.connect(InetSocketAddress("127.0.0.1", port), CONNECT_TIMEOUT_MS)
                s.tcpNoDelay = true
                s.soTimeout = HANDSHAKE_TIMEOUT_MS
                val input = DataInputStream(BufferedInputStream(s.getInputStream()))
                val output = s.getOutputStream()
                Log.i(tag, "socket connected to host on port $port")

                stage = "noise handshake"
                update("handshaking")
                val privateKey = TabletKeys.loadOrCreatePrivateKey(this)
                val deviceName = android.os.Build.MODEL ?: "tablet"
                NoiseSession.initiator(privateKey).use { noise ->
                    // XX over Noise records (docs/PROTOCOL.md): -> e,
                    // <- e, ee, s, es, -> s, se. Message sizes are
                    // never assumed; records are length-driven.
                    writeRecord(output, noise.writeMessage(ByteArray(0)))
                    Log.i(tag, "noise: initiator hello sent")
                    val record = ByteArray(Protocol.MAX_RECORD)
                    val len1 = readRecord(input, record)
                    Log.i(tag, "noise: host reply read ($len1 bytes)")
                    noise.readMessage(record.copyOf(len1))
                    writeRecord(output, noise.writeMessage(deviceName.toByteArray()))
                    Log.i(tag, "noise: tablet identity sent ('$deviceName')")

                    stage = "checking trusted hosts"
                    val hostKey = noise.remoteStatic()
                        ?: throw ProtocolException("host static key unknown after XX")
                    val code = pairingCode(noise.handshakeHash())
                    val trusted = TabletKeys.isTrustedHost(this, hostKey)
                    // The pairing wait is human-paced; the protocol
                    // exchange resumes without a read deadline. A cable
                    // pull still surfaces as an EOF.
                    s.soTimeout = 0
                    stage = "switching to encrypted transport"
                    noise.intoTransport()

                    // Pairing dance (docs/PROTOCOL.md, "Handshake and
                    // versioning"): a side that needs the code confirmed
                    // announces PairNeeded first, so the code is visible
                    // and confirmed on BOTH screens whenever either side
                    // requires pairing.
                    stage = "pairing exchange"
                    val channel = SecureChannel(noise)
                    if (!trusted) {
                        // This tablet does not know the host: announce it
                        // so the host shows its prompt too, then confirm
                        // here.
                        channel.writeFrame(
                            output,
                            FrameHeader(Protocol.CHANNEL_CONTROL, 0, Protocol.PAIR_NEEDED_TYPE, 0, 0),
                            ByteArray(0),
                        )
                        Log.i(tag, "pairing: tablet needs pairing, PairNeeded sent")
                        update("pairing: confirm code $code on host and tablet")
                        Log.i(tag, "pairing: prompting for code $code")
                        requestPairing(code)
                        Log.i(tag, "pairing: confirmed on the tablet")
                    }

                    stage = "hello exchange"
                    val hello = buildHello(
                        appVersion = packageManager.getPackageInfo(packageName, 0).versionName ?: "0.1.0",
                        deviceModel = deviceName,
                        androidApi = android.os.Build.VERSION.SDK_INT,
                    )
                    val helloBytes = hello.toByteArray()
                    var ack: HelloAck? = null
                    while (ack == null) {
                        channel.writeFrame(
                            output,
                            FrameHeader(Protocol.CHANNEL_CONTROL, 0, Protocol.HELLO_TYPE, helloBytes.size, 0),
                            helloBytes,
                        )
                        Log.i(tag, "hello: sent (app '${hello.appVersion}')")
                        val (frame, payload) = channel.readFrame(input)
                        if (frame.channel != Protocol.CHANNEL_CONTROL) {
                            throw ProtocolException(
                                "expected a control frame, got channel ${frame.channel}"
                            )
                        }
                        when (frame.type) {
                            Protocol.HELLO_ACK_TYPE -> ack = HelloAck.parseFrom(payload)
                            Protocol.PAIR_NEEDED_TYPE -> {
                                // The host does not know this tablet: it
                                // asked for the code confirmation even
                                // though the tablet trusts it.
                                Log.i(tag, "pairing: host requests pairing confirmation")
                                update("pairing: confirm code $code on host and tablet")
                                requestPairing(code)
                                Log.i(tag, "pairing: confirmed on the tablet")
                            }
                            Protocol.CLOSE_TYPE -> throw ProtocolException(
                                "host closed the session during pairing"
                            )
                            else -> throw ProtocolException(
                                "expected HelloAck or PairNeeded, got type ${frame.type}"
                            )
                        }
                    }
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
                    if (!trusted) {
                        // Pairing was confirmed; trust this host from
                        // now on.
                        TabletKeys.trustHost(this, hostKey)
                    }

                    stage = "session"
                    update(
                        "handshake complete" + ack.appVersion.takeIf { it.isNotEmpty() }?.let { " with $it" }.orEmpty()
                    )
                    Log.i(tag, "handshake complete with host app '${ack.appVersion}'")

                    // The M0 shell answers clock-sync pings and holds
                    // the session open; the daemon drives the echo.
                    var pongs = 0
                    while (true) {
                        val (frame, payload) = channel.readFrame(input)
                        if (frame.channel == Protocol.CHANNEL_CONTROL && frame.type == Protocol.PING_TYPE) {
                            // t1 stamps the ping's arrival and t2 the
                            // reply's send; stamping t1 any earlier
                            // would count idle waiting into the host's
                            // RTT math (docs/PROTOCOL.md, "Clocks").
                            val t1 = android.os.SystemClock.elapsedRealtimeNanos() / 1000
                            val pong = buildPong(payload, t1, android.os.SystemClock.elapsedRealtimeNanos() / 1000)
                            channel.writeFrame(
                                output,
                                FrameHeader(Protocol.CHANNEL_CONTROL, 0, Protocol.PONG_TYPE, pong.size, 0),
                                pong,
                            )
                            pongs++
                            if (pongs == 1) Log.i(tag, "session: first clock ping answered")
                        }
                        // Other frames are discarded for now.
                    }
                }
            }
        } catch (e: Exception) {
            // Never log frame or input contents (docs/SECURITY.md): the
            // exception class, its message and our own short messages
            // only.
            Log.e(tag, "session failed during stage: $stage", e)
            update("disconnected: ${e.javaClass.simpleName} ($stage)")
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
