// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.Button
import android.widget.EditText
import android.widget.TextView

/**
 * Minimal development shell (docs/ROADMAP.md, M0): pick the host port,
 * connect through adb reverse, watch the handshake status.
 */
class MainActivity : Activity() {

    private lateinit var status: TextView
    private val handler = Handler(Looper.getMainLooper())

    private val refreshStatus = object : Runnable {
        override fun run() {
            status.text = ConnectionService.statusText
            handler.postDelayed(this, STATUS_REFRESH_MS)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        status = findViewById(R.id.status)
        status.text = ConnectionService.statusText

        val portField = findViewById<EditText>(R.id.port)
        portField.setText(ConnectionService.DEFAULT_PORT.toString())

        findViewById<Button>(R.id.connect).setOnClickListener {
            if (android.os.Build.VERSION.SDK_INT >= 33) {
                requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
            }
            val port = portField.text.toString().toIntOrNull() ?: ConnectionService.DEFAULT_PORT
            val intent = Intent(this, ConnectionService::class.java)
                .putExtra(ConnectionService.EXTRA_PORT, port)
            startForegroundService(intent)
        }

        findViewById<Button>(R.id.disconnect).setOnClickListener {
            stopService(Intent(this, ConnectionService::class.java))
        }
    }

    override fun onResume() {
        super.onResume()
        handler.post(refreshStatus)
    }

    override fun onPause() {
        super.onPause()
        handler.removeCallbacks(refreshStatus)
    }

    private companion object {
        const val STATUS_REFRESH_MS = 250L
    }
}
