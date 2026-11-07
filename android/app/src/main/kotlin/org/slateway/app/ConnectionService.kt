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
        try {
            Socket().use { s ->
                synchronized(this) { socket = s }
                s.connect(InetSocketAddress("127.0.0.1", port), CONNECT_TIMEOUT_MS)
                s.tcpNoDelay = true
                s.soTimeout = HANDSHAKE_TIMEOUT_MS

                update("sending Hello")
                val appVersion = packageManager.getPackageInfo(packageName, 0).versionName ?: "0.1.0"
                val hello = buildHello(
                    appVersion = appVersion,
                    deviceModel = android.os.Build.MODEL ?: "unknown",
                    androidApi = android.os.Build.VERSION.SDK_INT,
                )
                writeFrame(s, Frame(Protocol.CHANNEL_CONTROL, 0, Protocol.HELLO_TYPE, hello.toByteArray(), 0))

                val input = DataInputStream(BufferedInputStream(s.getInputStream()))
                val ackFrame = Frame.readFrom(input)
                if (ackFrame.channel != Protocol.CHANNEL_CONTROL || ackFrame.type != Protocol.HELLO_ACK_TYPE) {
                    throw ProtocolException(
                        "expected HelloAck, got channel ${ackFrame.channel} type ${ackFrame.type}"
                    )
                }
                val ack = HelloAck.parseFrom(ackFrame.payload)
                if (ack.protocolMajor != Protocol.PROTOCOL_MAJOR) {
                    writeFrame(
                        s,
                        Frame(
                            Protocol.CHANNEL_CONTROL, 0, Protocol.CLOSE_TYPE,
                            Close.newBuilder()
                                .setReason(CloseReason.MAJOR_MISMATCH)
                                .build()
                                .toByteArray(),
                            0,
                        ),
                    )
                    throw ProtocolException(
                        "host protocol major ${ack.protocolMajor} does not match ${Protocol.PROTOCOL_MAJOR}"
                    )
                }

                s.soTimeout = 0
                update("handshake complete" + ack.appVersion.takeIf { it.isNotEmpty() }?.let { " with $it" }.orEmpty())

                // The M0 shell holds the session open and discards
                // further frames; the daemon drives the echo.
                while (true) {
                    Frame.readFrom(input)
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

    private fun writeFrame(socket: Socket, frame: Frame) {
        socket.getOutputStream().apply {
            write(frame.encode())
            flush()
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

        /** Connection status for the activity; contents never include
         * frame or input data (docs/SECURITY.md). */
        @Volatile
        var statusText: String = "disconnected"
            private set
    }
}
