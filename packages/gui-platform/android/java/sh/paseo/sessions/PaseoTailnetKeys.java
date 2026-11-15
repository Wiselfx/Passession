package sh.paseo.sessions;

import android.content.Context;
import android.os.Looper;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.system.Os;
import android.system.OsConstants;
import android.util.AtomicFile;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.KeyStore;
import java.security.SecureRandom;
import java.util.Arrays;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/**
 * AndroidKeystore's AES key never leaves its provider. It wraps the 32-byte
 * libtailscale state key; only that state key crosses JNI, never JSON or logs.
 * Wrapped blobs live outside Android backup so restored state cannot be opened
 * with a silently regenerated key. Rust holds the state directory's process
 * lock across every call and the whole lifetime of its embedded node.
 */
final class PaseoTailnetKeys {
    private static final String SERVICE = "sh.paseo.sessions.tailnet.state.v1";
    private static final int KEY_LENGTH = 32;
    private static final int IV_LENGTH = 12;
    private static final int BLOB_LENGTH = 1 + IV_LENGTH + KEY_LENGTH + 16;

    private PaseoTailnetKeys() {}

    static synchronized byte[] access(Context context, String identity, boolean delete,
                                      boolean hasState) throws Exception {
        if (Looper.myLooper() == Looper.getMainLooper()) {
            throw new IllegalStateException("Tailnet secure storage requires a worker thread");
        }
        if (identity == null || !identity.matches("[0-9a-f]{64}")) {
            throw new IllegalArgumentException("Invalid tailnet key namespace");
        }
        File directory = new File(context.getNoBackupFilesDir(), "tailnet-keys");
        if (!directory.isDirectory() && !directory.mkdir()) {
            throw new IOException("Cannot create private tailnet key directory");
        }
        restrict(directory, true);
        File path = new File(directory, identity + ".wrapped");
        File backup = new File(path.getPath() + ".bak");
        File pending = new File(path.getPath() + ".new");
        restrictIfPresent(path);
        restrictIfPresent(backup);
        restrictIfPresent(pending);
        AtomicFile file = new AtomicFile(path);
        String alias = SERVICE + "." + identity;
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);

        if (delete) {
            // Delete the wrapping key first: even if file removal then fails,
            // no recoverable key remains in the encrypted blob. Report either
            // failure; never claim that logout cleanup succeeded prematurely.
            if (store.containsAlias(alias)) store.deleteEntry(alias);
            if (store.containsAlias(alias)) {
                throw new GeneralSecurityException("Tailnet wrapping key deletion failed");
            }
            file.delete();
            if (path.exists() || backup.exists() || pending.exists()) {
                throw new IOException("Tailnet wrapped key deletion failed");
            }
            return null;
        }

        boolean blobExists = path.exists() || backup.exists();
        boolean aliasExists = store.containsAlias(alias);
        if (blobExists) {
            if (!aliasExists) {
                throw new GeneralSecurityException("Existing tailnet wrapping key is missing");
            }
            SecretKey wrapping = (SecretKey) store.getKey(alias, null);
            if (wrapping == null) {
                throw new GeneralSecurityException("Existing tailnet wrapping key is unavailable");
            }
            return unwrap(file, wrapping, alias);
        }
        // Neither a missing blob nor an invalidated alias authorizes replacing
        // the encryption key for an existing node.
        if (hasState) {
            throw new GeneralSecurityException("Wrapped key for existing tailnet state is missing");
        }
        SecretKey wrapping;
        if (aliasExists) {
            wrapping = (SecretKey) store.getKey(alias, null);
            if (wrapping == null) {
                throw new GeneralSecurityException("Tailnet wrapping key is unavailable");
            }
        } else {
            KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
            generator.init(new KeyGenParameterSpec.Builder(alias,
                    KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setKeySize(256)
                    .setRandomizedEncryptionRequired(true)
                    .setUserAuthenticationRequired(false)
                    .build());
            wrapping = generator.generateKey();
        }
        byte[] key = new byte[KEY_LENGTH];
        boolean returned = false;
        try {
            new SecureRandom().nextBytes(key);
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            // The provider chooses a fresh random IV; callers cannot force IV
            // reuse for this non-exportable encryption key.
            cipher.init(Cipher.ENCRYPT_MODE, wrapping);
            cipher.updateAAD(alias.getBytes(StandardCharsets.UTF_8));
            byte[] iv = cipher.getIV();
            if (iv.length != IV_LENGTH) throw new GeneralSecurityException("Unexpected GCM IV length");
            byte[] encrypted = cipher.doFinal(key);
            byte[] blob = new byte[BLOB_LENGTH];
            blob[0] = 1;
            System.arraycopy(iv, 0, blob, 1, IV_LENGTH);
            System.arraycopy(encrypted, 0, blob, 1 + IV_LENGTH, encrypted.length);
            FileOutputStream output = null;
            try {
                output = file.startWrite();
                restrictIfPresent(path);
                restrictIfPresent(backup);
                restrictIfPresent(pending);
                output.write(blob);
                output.getFD().sync();
                file.finishWrite(output);
                output = null;
            } finally {
                if (output != null) file.failWrite(output);
            }
            restrictIfPresent(path);
            byte[] saved = unwrap(file, wrapping, alias);
            try {
                if (!java.security.MessageDigest.isEqual(key, saved)) {
                    throw new GeneralSecurityException("Wrapped tailnet key readback failed");
                }
            } finally {
                Arrays.fill(saved, (byte) 0);
            }
            returned = true;
            return key;
        } finally {
            // On success Rust clears the returned byte[] immediately after
            // copying it into its zeroizing decode buffer.
            if (!returned) Arrays.fill(key, (byte) 0);
        }
    }

    private static byte[] unwrap(AtomicFile file, SecretKey wrapping, String alias)
            throws IOException, GeneralSecurityException {
        byte[] blob = new byte[BLOB_LENGTH + 1];
        int length = 0;
        try (FileInputStream input = file.openRead()) {
            while (length < blob.length) {
                int count = input.read(blob, length, blob.length - length);
                if (count == -1) break;
                length += count;
            }
        }
        if (length != BLOB_LENGTH || blob[0] != 1) {
            throw new GeneralSecurityException("Invalid wrapped tailnet key format");
        }
        Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
        cipher.init(Cipher.DECRYPT_MODE, wrapping, new GCMParameterSpec(128, blob, 1, IV_LENGTH));
        cipher.updateAAD(alias.getBytes(StandardCharsets.UTF_8));
        byte[] key = cipher.doFinal(blob, 1 + IV_LENGTH, KEY_LENGTH + 16);
        if (key.length != KEY_LENGTH) {
            Arrays.fill(key, (byte) 0);
            throw new GeneralSecurityException("Invalid tailnet state key length");
        }
        return key;
    }

    private static void restrictIfPresent(File path) throws Exception {
        // lstat, unlike exists(), also sees dangling symlinks. Only ENOENT is
        // absence; denied access is not mistaken for a missing credential.
        try {
            int mode = Os.lstat(path.getPath()).st_mode;
            if (!OsConstants.S_ISREG(mode)) throw new IOException("Tailnet key blob is not a regular file");
            Os.chmod(path.getPath(), 0600);
        } catch (android.system.ErrnoException error) {
            if (error.errno != OsConstants.ENOENT) throw error;
        }
    }

    private static void restrict(File path, boolean directory) throws Exception {
        int mode = Os.lstat(path.getPath()).st_mode;
        if (directory ? !OsConstants.S_ISDIR(mode) : !OsConstants.S_ISREG(mode)) {
            throw new IOException("Invalid private tailnet key path");
        }
        Os.chmod(path.getPath(), directory ? 0700 : 0600);
    }
}
