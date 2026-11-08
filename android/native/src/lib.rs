// SPDX-License-Identifier: GPL-3.0-or-later

//! JNI facade over the Noise state machine (ADR-013): the tablet app
//! drives `snow` through this small surface and owns all I/O itself
//! (`docs/PROTOCOL.md`, "Noise records"). The host runs the same crate
//! version on the other end, so the crypto is identical by
//! construction.
//!
//! Session pointers are `Box<NoiseSession>` behind a `jlong`. Every
//! dereference is an `unsafe` block with a `// SAFETY:` comment; the
//! invariant is that Kotlin only ever passes back pointers this
//! library handed out and never uses one after `freeSession`.

use jni::objects::{JByteArray, JClass, JObject};
use jni::JNIEnv;
use snow::{Builder, HandshakeState, TransportState};

/// The Noise protocol limit; every ciphertext/payload passed across
/// the boundary must fit.
const MAX_MESSAGE: usize = 65535;

/// The cipher suite fixed by ADR-006 and docs/PROTOCOL.md; the Kotlin
/// side pins the same string.
const CIPHER_SUITE: &str = "Noise_XX_25519_ChaChaPoly_SHA256";

/// Which half of the Noise state machine a session pointer holds.
/// Boxed at the JNI boundary, so the size difference between the
/// variants does not matter.
#[allow(clippy::large_enum_variant)]
enum NoiseSession {
    Handshake(HandshakeState),
    Transport(TransportState),
}

/// Reports `msg` as a Java IllegalStateException; used for every error
/// path so Kotlin sees a normal exception.
fn throw(env: &mut JNIEnv, msg: &str) {
    // Best effort: if throwing fails there is nothing left to do.
    let _ = env.throw_new("java/lang/IllegalStateException", msg);
}

/// Copies a Java byte array into Rust. Throws and returns empty on
/// failure.
fn read_bytes(env: &mut JNIEnv, array: &JByteArray, what: &str) -> Vec<u8> {
    match env.convert_byte_array(array) {
        Ok(bytes) => bytes,
        Err(err) => {
            throw(env, &format!("{what}: {err}"));
            Vec::new()
        }
    }
}

/// Copies Rust bytes into a fresh Java byte array. Throws and returns
/// null on failure.
fn write_bytes<'local>(env: &mut JNIEnv<'local>, bytes: &[u8]) -> JByteArray<'local> {
    match env.byte_array_from_slice(bytes) {
        Ok(array) => array,
        Err(err) => {
            throw(env, &format!("byte array out: {err}"));
            JByteArray::from(JObject::null())
        }
    }
}

/// # Safety
///
/// `ptr` must be a pointer this library returned from `newInitiator`,
/// `newResponder` or `intoTransport`, and must not have been passed to
/// `freeSession` yet. The Kotlin side keeps exactly one pointer per
/// session and closes it exactly once.
unsafe fn session_from_ptr<'a>(env: &mut JNIEnv, ptr: i64, env_name: &str) -> &'a mut NoiseSession {
    let raw = ptr as usize as *mut NoiseSession;
    // SAFETY: the pointer originated from Box::into_raw in this
    // library; Kotlin honors the single-owner, no-use-after-free
    // contract documented above.
    match unsafe { raw.as_mut() } {
        Some(session) => session,
        None => {
            throw(env, &format!("{env_name}: null session pointer"));
            // Only reachable on a programming error on the Kotlin side;
            // dereferencing null would be worse, so panic loudly.
            panic!("null Noise session pointer passed to {env_name}");
        }
    }
}

/// # Safety
///
/// Takes ownership back: `ptr` must satisfy the same contract as
/// [`session_from_ptr`] and must not be used again afterwards.
unsafe fn take_session(ptr: i64) -> Box<NoiseSession> {
    let raw = ptr as usize as *mut NoiseSession;
    // SAFETY: reconstructing the Box restores the allocation this
    // library created; Kotlin never reuses a pointer after free.
    unsafe { Box::from_raw(raw) }
}

fn handshake<'a>(env: &mut JNIEnv, session: &'a mut NoiseSession, what: &str) -> Option<&'a mut HandshakeState> {
    match session {
        NoiseSession::Handshake(state) => Some(state),
        NoiseSession::Transport(_) => {
            throw(env, &format!("{what}: session is already in transport mode"));
            None
        }
    }
}

fn transport<'a>(env: &mut JNIEnv, session: &'a mut NoiseSession, what: &str) -> Option<&'a mut TransportState> {
    match session {
        NoiseSession::Transport(state) => Some(state),
        NoiseSession::Handshake(_) => {
            throw(env, &format!("{what}: handshake is not finished"));
            None
        }
    }
}

/// Generates a static keypair; returns `private || public` (32 + 32
/// bytes).
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_generateKeypair<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> JByteArray<'local> {
    let params = match CIPHER_SUITE.parse() {
        Ok(params) => params,
        Err(err) => {
            throw(&mut env, &format!("cipher suite: {err}"));
            return JByteArray::from(JObject::null());
        }
    };
    match Builder::new(params).generate_keypair() {
        Ok(keypair) => {
            let mut both = keypair.private;
            both.extend_from_slice(&keypair.public);
            write_bytes(&mut env, &both)
        }
        Err(err) => {
            throw(&mut env, &format!("keypair: {err}"));
            JByteArray::from(JObject::null())
        }
    }
}

/// Creates an initiator (the tablet's role) from a 32-byte private
/// key; returns a session pointer.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_newInitiator<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    private_key: JByteArray<'local>,
) -> i64 {
    new_session(&mut env, &private_key, true, "newInitiator")
}

/// Creates a responder (the host's role) from a 32-byte private key;
/// returns a session pointer.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_newResponder<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    private_key: JByteArray<'local>,
) -> i64 {
    new_session(&mut env, &private_key, false, "newResponder")
}

fn new_session(env: &mut JNIEnv, private_key: &JByteArray, initiator: bool, what: &str) -> i64 {
    let private = read_bytes(env, private_key, what);
    let mut private_array = [0u8; 32];
    if private.len() != 32 {
        throw(env, &format!("{what}: private key must be 32 bytes, got {}", private.len()));
        return 0;
    }
    private_array.copy_from_slice(&private);
    let params = match CIPHER_SUITE.parse() {
        Ok(params) => params,
        Err(err) => {
            throw(env, &format!("cipher suite: {err}"));
            return 0;
        }
    };
    let built = match Builder::new(params).local_private_key(&private_array) {
        Ok(builder) => builder,
        Err(err) => {
            throw(env, &format!("{what}: {err}"));
            return 0;
        }
    };
    let state = if initiator {
        built.build_initiator()
    } else {
        built.build_responder()
    };
    match state {
        Ok(state) => {
            // SAFETY: the boxed session is handed to Kotlin as an opaque
            // long and only ever reclaimed through freeSession/take.
            Box::into_raw(Box::new(NoiseSession::Handshake(state))) as i64
        }
        Err(err) => {
            throw(env, &format!("{what}: {err}"));
            0
        }
    }
}

/// Handshake write: returns the ciphertext of the next handshake
/// message carrying `payload`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_writeMessage<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: i64,
    payload: JByteArray<'local>,
) -> JByteArray<'local> {
    let payload_bytes = read_bytes(&mut env, &payload, "writeMessage");
    if payload_bytes.len() > MAX_MESSAGE {
        throw(&mut env, "writeMessage: payload exceeds the Noise limit");
        return JByteArray::from(JObject::null());
    }
    // SAFETY: pointer contract documented on session_from_ptr.
    let session = unsafe { session_from_ptr(&mut env, ptr, "writeMessage") };
    let Some(state) = handshake(&mut env, session, "writeMessage") else {
        return JByteArray::from(JObject::null());
    };
    let mut out = [0u8; MAX_MESSAGE];
    match state.write_message(&payload_bytes, &mut out) {
        Ok(len) => write_bytes(&mut env, &out[..len]),
        Err(err) => {
            throw(&mut env, &format!("writeMessage: {err}"));
            JByteArray::from(JObject::null())
        }
    }
}

/// Handshake read: processes `message` and returns the decrypted
/// payload.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_readMessage<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: i64,
    message: JByteArray<'local>,
) -> JByteArray<'local> {
    let message_bytes = read_bytes(&mut env, &message, "readMessage");
    if message_bytes.len() > MAX_MESSAGE {
        throw(&mut env, "readMessage: message exceeds the Noise limit");
        return JByteArray::from(JObject::null());
    }
    // SAFETY: pointer contract documented on session_from_ptr.
    let session = unsafe { session_from_ptr(&mut env, ptr, "readMessage") };
    let Some(state) = handshake(&mut env, session, "readMessage") else {
        return JByteArray::from(JObject::null());
    };
    let mut out = [0u8; MAX_MESSAGE];
    match state.read_message(&message_bytes, &mut out) {
        Ok(len) => write_bytes(&mut env, &out[..len]),
        Err(err) => {
            throw(&mut env, &format!("readMessage: {err}"));
            JByteArray::from(JObject::null())
        }
    }
}

/// The 32-byte handshake hash; the pairing code derives from it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_handshakeHash<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: i64,
) -> JByteArray<'local> {
    // SAFETY: pointer contract documented on session_from_ptr.
    let session = unsafe { session_from_ptr(&mut env, ptr, "handshakeHash") };
    match session {
        NoiseSession::Handshake(state) => write_bytes(&mut env, state.get_handshake_hash()),
        NoiseSession::Transport(_) => {
            throw(&mut env, "handshakeHash: session is in transport mode");
            JByteArray::from(JObject::null())
        }
    }
}

/// The remote party's static public key, or null before it is known.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_remoteStatic<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: i64,
) -> JByteArray<'local> {
    // SAFETY: pointer contract documented on session_from_ptr.
    let session = unsafe { session_from_ptr(&mut env, ptr, "remoteStatic") };
    match session {
        NoiseSession::Handshake(state) => match state.get_remote_static() {
            Some(key) => write_bytes(&mut env, key),
            None => JByteArray::from(JObject::null()),
        },
        NoiseSession::Transport(_) => {
            throw(&mut env, "remoteStatic: session is in transport mode");
            JByteArray::from(JObject::null())
        }
    }
}

/// Finishes the handshake; returns the new transport-mode session
/// pointer. The old pointer is consumed.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_intoTransport<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: i64,
) -> i64 {
    // SAFETY: the pointer is consumed; Kotlin replaces it with the
    // returned one and never touches the old value again.
    let boxed = unsafe { take_session(ptr) };
    match *boxed {
        NoiseSession::Handshake(state) => match state.into_transport_mode() {
            Ok(transport) => {
                // SAFETY: same handoff contract as above.
                Box::into_raw(Box::new(NoiseSession::Transport(transport))) as i64
            }
            Err(err) => {
                throw(&mut env, &format!("intoTransport: {err}"));
                0
            }
        },
        NoiseSession::Transport(_) => {
            throw(&mut env, "intoTransport: session is already in transport mode");
            0
        }
    }
}

/// Transport-mode encryption of one record plaintext.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_encrypt<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: i64,
    plaintext: JByteArray<'local>,
) -> JByteArray<'local> {
    transport_crypto(&mut env, ptr, &plaintext, true, "encrypt")
}

/// Transport-mode decryption of one record ciphertext.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_decrypt<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: i64,
    ciphertext: JByteArray<'local>,
) -> JByteArray<'local> {
    transport_crypto(&mut env, ptr, &ciphertext, false, "decrypt")
}

fn transport_crypto<'local>(
    env: &mut JNIEnv<'local>,
    ptr: i64,
    input: &JByteArray<'local>,
    encrypting: bool,
    what: &str,
) -> JByteArray<'local> {
    let bytes = read_bytes(env, input, what);
    if bytes.len() > MAX_MESSAGE {
        throw(env, &format!("{what}: input exceeds the Noise limit"));
        return JByteArray::from(JObject::null());
    }
    // SAFETY: pointer contract documented on session_from_ptr.
    let session = unsafe { session_from_ptr(env, ptr, what) };
    let Some(state) = transport(env, session, what) else {
        return JByteArray::from(JObject::null());
    };
    let mut out = [0u8; MAX_MESSAGE];
    let result = if encrypting {
        state.write_message(&bytes, &mut out)
    } else {
        state.read_message(&bytes, &mut out)
    };
    match result {
        Ok(len) => write_bytes(env, &out[..len]),
        Err(err) => {
            throw(env, &format!("{what}: {err}"));
            JByteArray::from(JObject::null())
        }
    }
}

/// Releases a session pointer. The pointer must not be used again.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_slateway_app_NativeNoise_freeSession<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    ptr: i64,
) {
    if ptr == 0 {
        return;
    }
    // SAFETY: the pointer is reclaimed exactly once; Kotlin guarantees
    // single close per session.
    let boxed = unsafe { take_session(ptr) };
    drop(boxed);
}
