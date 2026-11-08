// SPDX-License-Identifier: GPL-3.0-or-later

package org.slateway.app

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.TextView

/**
 * Minimal development shell (docs/ROADMAP.md, M0): pick the host port,
 * connect through adb reverse, confirm the pairing code, watch the
 * handshake status.
 */
class MainActivity : Activity() {

    private lateinit var status: TextView
    private lateinit var pairButton: Button
    private val handler = Handler(Looper.getMainLooper())

    private val refreshStatus = object : Runnable {
        override fun run() {
            status.text = ConnectionService.statusText
            val code = ConnectionService.pairing.code
            pairButton.visibility = if (code != null) View.VISIBLE else View.GONE
            if (code != null) {
                pairButton.text = getString(R.string.pair, code)
            }
            handler.postDelayed(this, STATUS_REFRESH_MS)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        status = findViewById(R.id.status)
        status.text = ConnectionService.statusText
        pairButton = findViewById(R.id.pair)
        pairButton.visibility = View.GONE
        pairButton.setOnClickListener {
            ConnectionService.pairing.confirm()
        }

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
            ConnectionService.pairing.cancel()
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
