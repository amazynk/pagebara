package com.amazynk.pagebara;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.ContentResolver;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.res.Configuration;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayDeque;

/**
 * Hosts the Rust app (android_main in libpagebara_mobile.so) and does what needs the Android
 * APIs: the document and photo pickers, copying picked documents into the app, writing
 * saved documents back, saving copies and sharing. Results are queued as events
 * ("open\n/path", "image\n/path", "data\n/path", "message\ntext") that Rust fetches with
 * nextEvent().
 */
public class MainActivity extends NativeActivity {
    private static final int PICK_PDF = 1;
    private static final int PICK_IMAGE = 2;
    private static final int SAVE_COPY = 3;
    private static final int PICK_DATA = 4;
    /** Where each local copy of a document from another app came from. */
    private static final String ORIGINS = "origins";

    private final ArrayDeque<String> events = new ArrayDeque<>();
    private String pendingCopy;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        received(getIntent());
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        received(intent);
    }

    /** A document opened from another app. */
    private void received(Intent intent) {
        if (intent == null) {
            return;
        }
        Uri uri = null;
        if (Intent.ACTION_VIEW.equals(intent.getAction())) {
            uri = intent.getData();
        } else if (Intent.ACTION_SEND.equals(intent.getAction())) {
            uri = intent.getParcelableExtra(Intent.EXTRA_STREAM);
        }
        if (uri != null) {
            boolean writable = (intent.getFlags() & Intent.FLAG_GRANT_WRITE_URI_PERMISSION) != 0;
            importDocument(uri, writable);
        }
        intent.setAction(Intent.ACTION_MAIN);
    }

    private synchronized void post(String kind, String value) {
        events.add(kind + "\n" + value);
    }

    /** The next event for Rust, or null. */
    public synchronized String nextEvent() {
        return events.poll();
    }

    public String cacheDir() {
        return getCacheDir().getAbsolutePath();
    }

    public boolean isDark() {
        int night = getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        return night == Configuration.UI_MODE_NIGHT_YES;
    }

    public void pickPdf() {
        Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        i.addCategory(Intent.CATEGORY_OPENABLE);
        i.setType("application/pdf");
        i.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION
                | Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
        start(i, PICK_PDF);
    }

    public void pickImage() {
        Intent i = new Intent(Intent.ACTION_GET_CONTENT);
        i.addCategory(Intent.CATEGORY_OPENABLE);
        i.setType("image/*");
        start(i, PICK_IMAGE);
    }

    public void pickData() {
        Intent i = new Intent(Intent.ACTION_GET_CONTENT);
        i.addCategory(Intent.CATEGORY_OPENABLE);
        i.setType("*/*");
        i.putExtra(Intent.EXTRA_MIME_TYPES, new String[] {
            "application/vnd.fdf", "application/vnd.adobe.xfdf", "application/xml",
            "text/xml", "application/octet-stream"});
        start(i, PICK_DATA);
    }

    public void saveCopy(String path, String name) {
        pendingCopy = path;
        Intent i = new Intent(Intent.ACTION_CREATE_DOCUMENT);
        i.addCategory(Intent.CATEGORY_OPENABLE);
        i.setType(name.endsWith(".xfdf") ? "application/vnd.adobe.xfdf"
                : name.endsWith(".fdf") ? "application/vnd.fdf" : "application/pdf");
        i.putExtra(Intent.EXTRA_TITLE, name);
        start(i, SAVE_COPY);
    }

    /** Offers a file in cache/share to other apps through ShareProvider. */
    public void share(String path, String name) {
        Uri uri = ShareProvider.uriFor(this, new File(path));
        Intent send = new Intent(Intent.ACTION_SEND);
        send.setType("application/pdf");
        send.putExtra(Intent.EXTRA_STREAM, uri);
        send.putExtra(Intent.EXTRA_SUBJECT, name);
        send.setClipData(ClipData.newRawUri(name, uri));
        send.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        Intent chooser = Intent.createChooser(send, null);
        runOnUiThread(() -> {
            try {
                startActivity(chooser);
            } catch (Exception e) {
                post("message", "Nothing here can take the document.");
            }
        });
    }

    /**
     * After Rust saved the local copy at path: copies it to the document it came from.
     * 0: it is not a copy; 1: written back; 2: that failed.
     */
    public int writeBack(String path) {
        String origin = getSharedPreferences(ORIGINS, MODE_PRIVATE).getString(path, null);
        if (origin == null) {
            return 0;
        }
        try (InputStream in = new FileInputStream(path);
             OutputStream out = getContentResolver().openOutputStream(Uri.parse(origin), "wt")) {
            if (out == null) {
                return 2;
            }
            copy(in, out);
            return 1;
        } catch (Exception e) {
            return 2;
        }
    }

    private void start(Intent i, int request) {
        runOnUiThread(() -> {
            try {
                startActivityForResult(i, request);
            } catch (Exception e) {
                post("message", "No app here can do that.");
            }
        });
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        Uri uri = data == null ? null : data.getData();
        if (result != RESULT_OK || uri == null) {
            return;
        }
        if (request == PICK_PDF) {
            try {
                getContentResolver().takePersistableUriPermission(uri,
                        Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION);
            } catch (Exception e) {
                // Read once only; saving goes to a copy.
            }
            importDocument(uri, true);
        } else if (request == PICK_IMAGE) {
            importFile(uri, new File(getCacheDir(), "photos"), "image", false);
        } else if (request == PICK_DATA) {
            importFile(uri, new File(getCacheDir(), "data"), "data", false);
        } else if (request == SAVE_COPY && pendingCopy != null) {
            String from = pendingCopy;
            pendingCopy = null;
            new Thread(() -> {
                try (InputStream in = new FileInputStream(from);
                     OutputStream out = getContentResolver().openOutputStream(uri, "wt")) {
                    copy(in, out);
                    post("message", "Saved a copy.");
                } catch (Exception e) {
                    post("message", "The copy could not be saved: " + e.getMessage());
                }
            }).start();
        }
    }

    private void importDocument(Uri uri, boolean writable) {
        if (ContentResolver.SCHEME_FILE.equals(uri.getScheme()) && uri.getPath() != null) {
            post("open", uri.getPath());
            return;
        }
        importFile(uri, new File(getFilesDir(), "documents"), "open", writable);
    }

    /** Copies uri into dir on a worker thread, then posts the copy's path as kind. */
    private void importFile(Uri uri, File dir, String kind, boolean remember) {
        new Thread(() -> {
            try {
                if (!dir.isDirectory() && !dir.mkdirs()) {
                    throw new IOException("cannot make " + dir);
                }
                File to = new File(dir, safeName(displayName(uri)));
                try (InputStream in = getContentResolver().openInputStream(uri);
                     OutputStream out = new FileOutputStream(to)) {
                    if (in == null) {
                        throw new IOException("nothing to read");
                    }
                    copy(in, out);
                }
                SharedPreferences.Editor origins = getSharedPreferences(ORIGINS, MODE_PRIVATE).edit();
                if (remember) {
                    origins.putString(to.getAbsolutePath(), uri.toString());
                } else {
                    origins.remove(to.getAbsolutePath());
                }
                origins.apply();
                post(kind, to.getAbsolutePath());
            } catch (Exception e) {
                post("message", "The file could not be read: " + e.getMessage());
            }
        }).start();
    }

    private String displayName(Uri uri) {
        try (Cursor c = getContentResolver().query(uri,
                new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0)) {
                return c.getString(0);
            }
        } catch (Exception e) {
            // Fall back to the path.
        }
        String last = uri.getLastPathSegment();
        return last == null ? "document.pdf" : last;
    }

    private static String safeName(String name) {
        String s = name.replaceAll("[/\\\\:*?\"<>|\\x00-\\x1f]", "_").trim();
        return s.isEmpty() || s.startsWith(".") ? "document" + s : s;
    }

    private static void copy(InputStream in, OutputStream out) throws IOException {
        byte[] buf = new byte[1 << 16];
        int n;
        while ((n = in.read(buf)) > 0) {
            out.write(buf, 0, n);
        }
    }
}
