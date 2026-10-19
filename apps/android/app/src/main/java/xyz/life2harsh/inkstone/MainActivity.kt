package xyz.life2harsh.inkstone

import android.annotation.SuppressLint
import android.content.ActivityNotFoundException
import android.content.Intent
import android.graphics.Color
import android.net.ConnectivityManager
import android.net.Network
import android.net.Uri
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.webkit.CookieManager
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.isVisible
import androidx.webkit.WebSettingsCompat
import androidx.webkit.WebViewFeature

class MainActivity : ComponentActivity() {
    private val appUri: Uri = Uri.parse(BuildConfig.APP_URL)
    private lateinit var web: WebView
    private lateinit var offline: View
    private var pendingFiles: ValueCallback<Array<Uri>>? = null
    private var revealed = false

    private val pickFiles =
        registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
            pendingFiles?.onReceiveValue(WebChromeClient.FileChooserParams.parseResult(result.resultCode, result.data))
            pendingFiles = null
        }

    private val network = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            runOnUiThread { if (offline.isVisible) retry() }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)

        val background = ContextCompat.getColor(this, R.color.page_background)
        val root = FrameLayout(this).apply { setBackgroundColor(background) }
        web = buildWebView(background)
        offline = buildOfflineView()
        root.addView(web, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))
        root.addView(offline, FrameLayout.LayoutParams(MATCH_PARENT, MATCH_PARENT))
        setContentView(root)

        ViewCompat.setOnApplyWindowInsetsListener(root) { view, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
            view.setPadding(bars.left, bars.top, bars.right, maxOf(bars.bottom, ime.bottom))
            WindowInsetsCompat.CONSUMED
        }

        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (web.canGoBack()) {
                    web.goBack()
                } else {
                    isEnabled = false
                    onBackPressedDispatcher.onBackPressed()
                    isEnabled = true
                }
            }
        })

        getSystemService(ConnectivityManager::class.java)?.registerDefaultNetworkCallback(network)

        if (savedInstanceState == null || web.restoreState(savedInstanceState) == null) {
            web.loadUrl(linkFrom(intent) ?: appUri.toString())
        }
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun buildWebView(background: Int) = WebView(this).apply {
        setBackgroundColor(background)
        alpha = 0f
        overScrollMode = View.OVER_SCROLL_NEVER
        isVerticalScrollBarEnabled = false
        isHorizontalScrollBarEnabled = false
        setRendererPriorityPolicy(WebView.RENDERER_PRIORITY_IMPORTANT, true)
        WebView.setWebContentsDebuggingEnabled(BuildConfig.DEBUG)

        settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            databaseEnabled = true
            mediaPlaybackRequiresUserGesture = false
            cacheMode = WebSettings.LOAD_DEFAULT
            offscreenPreRaster = true
            setSupportZoom(false)
            builtInZoomControls = false
            displayZoomControls = false
            allowFileAccess = false
            allowContentAccess = true
            userAgentString = "$userAgentString InkstoneAndroid/${BuildConfig.VERSION_NAME}"
        }
        if (WebViewFeature.isFeatureSupported(WebViewFeature.ALGORITHMIC_DARKENING)) {
            WebSettingsCompat.setAlgorithmicDarkeningAllowed(settings, false)
        }
        val view = this
        CookieManager.getInstance().apply {
            setAcceptCookie(true)
            setAcceptThirdPartyCookies(view, false)
        }

        webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                val url = request.url
                if (url.host == appUri.host) return false
                openExternally(url)
                return true
            }

            override fun onPageCommitVisible(view: WebView, url: String) {
                reveal()
            }

            override fun onPageFinished(view: WebView, url: String) {
                reveal()
            }

            override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) {
                if (request.isForMainFrame) {
                    offline.isVisible = true
                }
            }
        }

        webChromeClient = object : WebChromeClient() {
            override fun onShowFileChooser(
                view: WebView,
                callback: ValueCallback<Array<Uri>>,
                params: FileChooserParams,
            ): Boolean {
                pendingFiles?.onReceiveValue(null)
                pendingFiles = callback
                return try {
                    pickFiles.launch(params.createIntent())
                    true
                } catch (_: ActivityNotFoundException) {
                    pendingFiles = null
                    false
                }
            }
        }
    }

    private fun buildOfflineView(): View {
        val ink = ContextCompat.getColor(this, R.color.ink)
        val muted = ContextCompat.getColor(this, R.color.ink_muted)
        val density = resources.displayMetrics.density
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
            isVisible = false
            setBackgroundColor(ContextCompat.getColor(context, R.color.page_background))
            val pad = (32 * density).toInt()
            setPadding(pad, pad, pad, pad)
            addView(TextView(context).apply {
                text = getString(R.string.offline_title)
                textSize = 20f
                setTextColor(ink)
                gravity = Gravity.CENTER
            })
            addView(TextView(context).apply {
                text = getString(R.string.offline_body)
                textSize = 15f
                setTextColor(muted)
                gravity = Gravity.CENTER
                setPadding(0, (8 * density).toInt(), 0, (20 * density).toInt())
            })
            addView(Button(context).apply {
                text = getString(R.string.retry)
                isAllCaps = false
                setTextColor(Color.WHITE)
                setBackgroundColor(ContextCompat.getColor(context, R.color.mark))
                setOnClickListener { retry() }
            }, LinearLayout.LayoutParams(WRAP_CONTENT, WRAP_CONTENT))
        }
    }

    private fun reveal() {
        if (revealed) return
        revealed = true
        web.animate().alpha(1f).setDuration(160).start()
    }

    private fun retry() {
        offline.isVisible = false
        if (web.url.isNullOrEmpty() || web.url == "about:blank") {
            web.loadUrl(appUri.toString())
        } else {
            web.reload()
        }
    }

    private fun linkFrom(intent: Intent?): String? =
        intent?.data?.takeIf { intent.action == Intent.ACTION_VIEW && it.host == appUri.host }?.toString()

    private fun openExternally(uri: Uri) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, uri))
        } catch (_: ActivityNotFoundException) {
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        val link = linkFrom(intent) ?: return
        val current = web.url?.let(Uri::parse)
        web.loadUrl(link)
        if (current != null && current.path == Uri.parse(link).path) web.reload()
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        web.saveState(outState)
    }

    override fun onResume() {
        super.onResume()
        web.onResume()
    }

    override fun onPause() {
        CookieManager.getInstance().flush()
        web.onPause()
        super.onPause()
    }

    override fun onDestroy() {
        getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(network)
        web.destroy()
        super.onDestroy()
    }
}
