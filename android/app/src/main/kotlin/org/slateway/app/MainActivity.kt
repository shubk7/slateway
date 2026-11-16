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
    private lateinit var videoSurface: android.view.SurfaceView
    private lateinit var root: android.view.View
    private lateinit var videoFrame: android.view.View
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
        root = findViewById(R.id.root)
        videoFrame = findViewById(R.id.videoFrame)
        status = findViewById(R.id.status)
        status.text = ConnectionService.statusText
        videoSurface = findViewById(R.id.video)
        // Touch on the mirror surface drives the host pointer while a
        // session with input acceptance is live (M1 touch-as-pointer).
        videoSurface.setOnTouchListener { view, event ->
            ConnectionService.onVideoTouch(view, event)
        }
        videoSurface.holder.addCallback(object : android.view.SurfaceHolder.Callback {
            override fun surfaceCreated(holder: android.view.SurfaceHolder) {
                ConnectionService.videoSurfaceHolder = holder
                ConnectionService.attachVideoSurface(holder.surface)
            }

            override fun surfaceChanged(
                holder: android.view.SurfaceHolder,
                format: Int,
                width: Int,
                height: Int,
            ) {}

            override fun surfaceDestroyed(holder: android.view.SurfaceHolder) {
                ConnectionService.videoSurfaceHolder = null
                ConnectionService.detachVideoSurface()
            }
        })
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

        // Fullscreen mirror: the second-screen use case wants the whole
        // panel (the streamed display is the tablet's own resolution, so
        // fullscreen is also the exact-geometry 1:1 mapping). The
        // control panel hides; a small overlay button exits.
        findViewById<Button>(R.id.fullscreen).setOnClickListener { enterFullscreen() }
        findViewById<Button>(R.id.exitFullscreen).setOnClickListener { exitFullscreen() }
    }

    /** Hides the control panel and the system bars; the mirror surface
     * takes the whole screen. API 29 falls back to the legacy system
     * UI flags (WindowInsetsController is API 30). */
    private fun enterFullscreen() {
        findViewById<View>(R.id.controls).visibility = View.GONE
        findViewById<View>(R.id.exitFullscreen).visibility = View.VISIBLE
        root.setPadding(0, 0, 0, 0)
        videoFrameLinearLayoutParams().topMargin = 0
        videoFrame.requestLayout()
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(false)
            window.insetsController?.apply {
                systemBarsBehavior =
                    android.view.WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                hide(android.view.WindowInsets.Type.systemBars())
            }
        } else {
            @Suppress("DEPRECATION")
            window.decorView.systemUiVisibility =
                View.SYSTEM_UI_FLAG_LAYOUT_STABLE or
                View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION or
                View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or
                View.SYSTEM_UI_FLAG_HIDE_NAVIGATION or
                View.SYSTEM_UI_FLAG_FULLSCREEN or
                View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
        }
    }

    /** Restores the control panel and the system bars. */
    private fun exitFullscreen() {
        findViewById<View>(R.id.controls).visibility = View.VISIBLE
        findViewById<View>(R.id.exitFullscreen).visibility = View.GONE
        val pad = (24 * resources.displayMetrics.density).toInt()
        root.setPadding(pad, pad, pad, pad)
        videoFrameLinearLayoutParams().topMargin =
            (16 * resources.displayMetrics.density).toInt()
        videoFrame.requestLayout()
        if (android.os.Build.VERSION.SDK_INT >= 30) {
            window.setDecorFitsSystemWindows(true)
            window.insetsController?.show(android.view.WindowInsets.Type.systemBars())
        } else {
            @Suppress("DEPRECATION")
            window.decorView.systemUiVisibility = 0
        }
    }

    private fun videoFrameLinearLayoutParams() =
        videoFrame.layoutParams as android.widget.LinearLayout.LayoutParams

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
