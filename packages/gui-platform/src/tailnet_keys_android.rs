use super::{Result, decode_key};
use anyhow::{Context, ensure};
use jni::{
    JavaVM,
    objects::{JByteArray, JObject, JValue},
};

pub(super) fn load_or_create(identity: &str, has_state: bool) -> Result<[u8; 32]> {
    decode_key(call(identity, false, has_state)?)
}

pub(super) fn delete(identity: &str) -> Result<()> {
    call(identity, true, false).map(|_| ())
}

fn call(identity: &str, delete: bool, has_state: bool) -> Result<Vec<u8>> {
    let raw_vm = makepad_android_state::get_java_vm();
    ensure!(
        !raw_vm.is_null(),
        "Android Java VM is not available for secure storage"
    );
    let vm = unsafe { JavaVM::from_raw(raw_vm as *mut jni::sys::JavaVM) }
        .context("Cannot access Android Java VM for secure storage")?;
    // A guard detaches threads we attach; no permanent JVM attachment leaks on
    // Tokio's blocking pool. Local refs are released even on a Java exception.
    let mut env = vm
        .attach_current_thread()
        .context("Cannot attach Android secure storage worker")?;
    let result = env.with_local_frame(16, |env| -> jni::errors::Result<Vec<u8>> {
        let raw_activity = makepad_android_state::get_activity();
        if raw_activity.is_null() {
            return Err(jni::errors::Error::NullPtr("Android activity"));
        }
        // Makepad owns this global ref. JObject is a borrowed wrapper, not a
        // GlobalRef/AutoLocal, so dropping it never deletes the activity ref.
        let activity = unsafe { JObject::from_raw(raw_activity as jni::sys::jobject) };
        let namespace = env.new_string(identity)?;
        let value = env
            .call_method(
                &activity,
                "paseoTailnetKey",
                "(Ljava/lang/String;ZZ)[B",
                &[
                    JValue::Object(&namespace),
                    JValue::Bool(u8::from(delete)),
                    JValue::Bool(u8::from(has_state)),
                ],
            )?
            .l()?;
        if delete {
            return Ok(Vec::new());
        }
        let bytes = JByteArray::from(value);
        let key = env.convert_byte_array(&bytes);
        // Clear the Java heap copy before releasing the local frame. Only the
        // fixed-size key crosses JNI, never a JSON/string/base64 representation.
        let cleared = env.set_byte_array_region(&bytes, 0, &[0; 32]);
        match (key, cleared) {
            (Ok(key), Ok(())) => Ok(key),
            (Ok(mut key), Err(error)) => {
                use zeroize::Zeroize;
                key.zeroize();
                Err(error)
            }
            (Err(error), _) => Err(error),
        }
    });
    if env.exception_check().unwrap_or(true) {
        // In contrast to the general platform bridge, never describe/log a
        // credential exception. Provider messages may include sensitive data.
        let _ = env.exception_clear();
        anyhow::bail!(
            "Android Keystore access failed: unlock the device and retry; missing, invalidated or damaged existing keys are not replaced"
        );
    }
    result.context("Android secure key storage bridge failed")
}
