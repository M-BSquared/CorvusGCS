package de.corvusgcs.app;

import android.annotation.SuppressLint;
import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.SharedPreferences;
import android.graphics.Bitmap;
import android.net.http.SslError;
import android.os.Bundle;
import android.text.InputType;
import android.view.View;
import android.view.WindowManager;
import android.webkit.ConsoleMessage;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.EditText;

/**
 * Corvus GCS on Android: a full screen web view on a Corvus GCS server on the
 * network. The user agent carries CorvusAndroid, which is what makes the web
 * app start in tablet mode (src/js/touch.js); the operator can still switch it
 * off in Settings and that choice is kept.
 */
public class MainActivity extends Activity {
    private static final String PREFS = "corvus";
    private static final String KEY_SERVER = "server";

    private WebView web;
    private AlertDialog dialog;
    private String server;
    private boolean failed;

    @SuppressLint("SetJavaScriptEnabled")
    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);

        web = new WebView(this);
        setContentView(web);

        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);
        s.setMediaPlaybackRequiresUserGesture(false);
        s.setAllowFileAccess(false);
        s.setAllowContentAccess(false);
        s.setUserAgentString(s.getUserAgentString() + " CorvusAndroid/" + BuildConfig.VERSION_NAME);

        web.setWebChromeClient(new WebChromeClient() {
            @Override
            public boolean onConsoleMessage(ConsoleMessage m) {
                return true;
            }
        });
        web.setWebViewClient(new WebViewClient() {
            @Override
            public void onPageStarted(WebView v, String url, Bitmap icon) {
                failed = false;
            }

            @Override
            public void onReceivedError(WebView v, WebResourceRequest req, WebResourceError err) {
                if (req.isForMainFrame()) {
                    failed = true;
                    askRetry();
                }
            }

            @Override
            public void onReceivedSslError(WebView v, android.webkit.SslErrorHandler h, SslError e) {
                h.cancel();
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView v, WebResourceRequest req) {
                return !sameHost(req.getUrl().toString());
            }
        });

        SharedPreferences p = getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        server = p.getString(KEY_SERVER, BuildConfig.DEFAULT_SERVER);
        if (server == null || server.isEmpty()) askServer();
        else if (state != null) web.restoreState(state);
        else web.loadUrl(server);
    }

    private boolean sameHost(String url) {
        if (server == null) return false;
        return android.net.Uri.parse(url).getAuthority() != null
            && android.net.Uri.parse(url).getAuthority().equals(android.net.Uri.parse(server).getAuthority());
    }

    private static String normalise(String raw) {
        String t = raw.trim();
        if (t.isEmpty()) return "";
        if (!t.contains("://")) t = "http://" + t;
        if (!t.endsWith("/")) t += "/";
        return t;
    }

    private void askServer() {
        if (dialog != null) dialog.dismiss();
        final EditText input = new EditText(this);
        input.setInputType(InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_VARIATION_URI);
        input.setHint(R.string.server_hint);
        input.setSingleLine(true);
        if (server != null) input.setText(server);
        dialog = new AlertDialog.Builder(this)
            .setTitle(R.string.server_title)
            .setMessage(R.string.server_message)
            .setView(input)
            .setCancelable(server != null && !server.isEmpty())
            .setPositiveButton(R.string.connect, (d, w) -> {
                String url = normalise(input.getText().toString());
                if (url.isEmpty()) {
                    askServer();
                    return;
                }
                server = url;
                getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString(KEY_SERVER, url).apply();
                web.loadUrl(url);
            })
            .show();
    }

    private void askRetry() {
        if (dialog != null && dialog.isShowing()) return;
        dialog = new AlertDialog.Builder(this)
            .setMessage(getString(R.string.cannot_reach, server))
            .setCancelable(false)
            .setPositiveButton(R.string.retry, (d, w) -> web.loadUrl(server))
            .setNeutralButton(R.string.change_server, (d, w) -> askServer())
            .setNegativeButton(R.string.exit, (d, w) -> finish())
            .show();
    }

    private void immersive() {
        getWindow().getDecorView().setSystemUiVisibility(
            View.SYSTEM_UI_FLAG_LAYOUT_STABLE
            | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
            | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
            | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
            | View.SYSTEM_UI_FLAG_FULLSCREEN
            | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY);
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) immersive();
    }

    @Override
    public void onBackPressed() {
        if (web.canGoBack() && !failed) {
            web.goBack();
            return;
        }
        if (dialog != null && dialog.isShowing()) dialog.dismiss();
        dialog = new AlertDialog.Builder(this)
            .setTitle(R.string.leave_title)
            .setPositiveButton(R.string.exit, (d, w) -> finish())
            .setNeutralButton(R.string.change_server, (d, w) -> askServer())
            .setNegativeButton(android.R.string.cancel, null)
            .show();
    }

    @Override
    protected void onSaveInstanceState(Bundle out) {
        super.onSaveInstanceState(out);
        web.saveState(out);
    }

    @Override
    protected void onResume() {
        super.onResume();
        web.onResume();
    }

    @Override
    protected void onPause() {
        web.onPause();
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        if (dialog != null) dialog.dismiss();
        web.stopLoading();
        web.destroy();
        super.onDestroy();
    }
}
