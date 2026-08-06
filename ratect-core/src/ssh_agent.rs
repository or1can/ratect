// Copyright 2026 Orican Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! A minimal in-process ssh-agent, serving a fixed set of private keys read
//! from files over a private Unix socket.
//!
//! This is what makes a `build_ssh` entry's `paths` work — forwarding
//! explicit private key files into an image build with no ssh-agent running
//! on the host at all, which is the normal situation in CI. BuildKit's
//! `sshforward` service bridges each forwarded stream to *anything that
//! speaks the ssh-agent protocol*, so serving that protocol ourselves is
//! indistinguishable from forwarding a real agent — which is precisely what
//! Go BuildKit's own `sshprovider` does with `x/crypto`'s `agent.NewKeyring`.
//! Private keys never leave this process: only signatures cross the socket.
//!
//! Deliberately **self-contained** — no configuration, engine, or Docker
//! types appear here, so this module could be lifted out and published (or
//! offered upstream to `bollard`) as-is. Same standing constraint the
//! `dockerignore` crate is kept under. That's also why nothing below names
//! Ratect in an error message.
//!
//! This is not a full agent. Two request types get real answers —
//! `SSH_AGENTC_REQUEST_IDENTITIES` and `SSH_AGENTC_SIGN_REQUEST` — and
//! everything else (add, remove, lock, unlock, smartcard, extensions)
//! answers `SSH_AGENT_FAILURE`, which is all an SSH *client* performing
//! public-key authentication ever needs.
//!
//! Wire formats and constants are from [RFC 9987](https://www.rfc-editor.org/rfc/rfc9987),
//! the SSH agent protocol.
//!
//! a minimal in-process ssh-agent
//! (`Keyring`), serving a `build_ssh` entry's `paths` private keys over a Unix
//! socket in a `0700` temporary directory — which is what lets `paths` work
//! with no agent running on the host at all, the normal CI case. BuildKit's
//! `sshforward` bridges each forwarded stream to *anything* speaking the agent
//! protocol, so serving it ourselves is indistinguishable from forwarding a
//! real agent (exactly what Go BuildKit's own `sshprovider` does with
//! `x/crypto`'s `agent.NewKeyring`). Wire formats come from
//! [RFC 9987](https://www.rfc-editor.org/rfc/rfc9987); only
//! `REQUEST_IDENTITIES` and `SIGN_REQUEST` get real answers and everything
//! else returns `SSH_AGENT_FAILURE`, which is all an SSH *client* doing
//! public-key auth needs. Things to preserve when touching it, all from
//! [decisions/0005](https://github.com/or1can/ratect/blob/main/decisions/0005-build-ssh-keyring-placement.md): it stays
//! **extractable** — no config/engine/Docker types, and no error message
//! naming Ratect, so it could be lifted out or offered upstream to `bollard`
//! as a copy rather than a rewrite; private keys never cross the socket, only
//! signatures; and the socket is protected *twice* — an
//! unpredictably-named `0700` directory (the system temporary directory is
//! world-writable) plus the socket file itself chmod'ed to `0600` after
//! binding, since `bind` otherwise leaves it at the process umask. That
//! pairing is OpenSSH's own (`mkdtemp` + `umask(0177)` around the bind);
//! doing only the directory rests the whole protection on one `mode`
//! argument. It's a `chmod` rather than a umask because the umask is
//! process-global and this process is multi-threaded. Its accept loop owns
//! connections in a `tokio::task::JoinSet` rather than detaching them, so
//! dropping the `Keyring` aborts the connections it is *already* serving and
//! not merely the loop — an established client doesn't care that the socket
//! file has been unlinked. That matches the convention every other
//! `tokio::spawn` in `ratect-core` already follows (the handle is owned and
//! aborted — `stdin_pump`, `spawn_resize_listener`); a bare detached spawn is
//! the thing to look twice at. The loop also never gives up on an `accept`
//! error: most are per-connection or transient, and quietly serving nothing
//! for the rest of a build surfaces much later as an unexplained
//! authentication failure. Worth knowing that
//! Go BuildKit needs *neither*: its `sshprovider` serves a keyring over an
//! in-memory `net.Pipe()`, so no socket exists on the filesystem at all —
//! Ratect needs one only because the fork's `SshAgentSource` is
//! socket-based. Two non-obvious
//! details: **`ssh-key` 0.6.7's own RSA conversion is broken** (it passes the
//! prime `p` twice instead of `p` and `q`, so *no* RSA key can be signed with
//! through its `Signer` impl either) — `rsa_private_key` rebuilds the key from
//! components to work around it, and the workaround goes when `ssh-key` 0.7 is
//! published and adopted; and the socket path length is checked before binding,
//! because `sun_path` is only 104 bytes on macOS and its per-user `TMPDIR`
//! already spends about half of that. Its tests build every request from RFC
//! 9987's own numbers rather than from this module's constants — a test that
//! derives its input from the code can't validate a constant that crosses a
//! protocol boundary. `docker.rs`'s `classify_ssh_agent_paths` decides *when*
//! a keyring is needed (see below); this module knows nothing about
//! `build_ssh`.

use std::io::ErrorKind;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use signature::{RandomizedSigner, SignatureEncoding, Signer};
use ssh_key::private::{KeypairData, RsaKeypair};
use ssh_key::{Algorithm, HashAlg, PrivateKey, Signature};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

/// Generic failure response — the answer to every request this agent
/// doesn't implement, and to any request it can't satisfy (RFC 9987 §5.1).
const SSH_AGENT_FAILURE: u8 = 5;
/// "List the keys you hold" (RFC 9987 §5.2). No contents.
const SSH_AGENTC_REQUEST_IDENTITIES: u8 = 11;
/// The answer to [`SSH_AGENTC_REQUEST_IDENTITIES`] (RFC 9987 §5.2).
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;
/// "Sign this data with the key having this public blob" (RFC 9987 §5.3).
const SSH_AGENTC_SIGN_REQUEST: u8 = 13;
/// The answer to [`SSH_AGENTC_SIGN_REQUEST`] (RFC 9987 §5.3).
const SSH_AGENT_SIGN_RESPONSE: u8 = 14;

/// Sign an RSA key's data under `rsa-sha2-256` rather than the legacy
/// SHA-1 `ssh-rsa` algorithm (RFC 9987 §5.3).
const SSH_AGENT_RSA_SHA2_256: u32 = 0x0000_0002;
/// Sign an RSA key's data under `rsa-sha2-512` (RFC 9987 §5.3).
const SSH_AGENT_RSA_SHA2_512: u32 = 0x0000_0004;

/// The largest request this agent will read, matching OpenSSH's own agent
/// limit. A sign request carries only a session-identifier-derived blob, so
/// anything approaching this is a broken or hostile client rather than a
/// legitimately large message — and without a ceiling, a four-byte length
/// prefix would let one allocate up to 4 GiB.
const MAX_MESSAGE_LENGTH: usize = 256 * 1024;

/// The socket's file name inside this agent's own private directory. Kept
/// short on purpose — see [`MAX_SOCKET_PATH_LENGTH`].
const SOCKET_FILE_NAME: &str = "agent";

/// Prefixes each agent's own directory, so a stray one is attributable to
/// whoever left it behind. **The only place in this module that names the
/// program embedding it** — deliberately a constant so lifting the module
/// out is a one-line change rather than a search for stray branding.
const DIRECTORY_NAME_PREFIX: &str = "ratect-ssh-";

/// A conservative ceiling on the socket path's length. A Unix socket
/// address is a fixed-size `sun_path` buffer — 104 bytes on macOS, 108 on
/// Linux — so a path longer than this fails to bind with an error that says
/// nothing about why. macOS's per-user `TMPDIR` is already ~50 characters
/// deep, which is what makes this worth checking rather than assuming.
const MAX_SOCKET_PATH_LENGTH: usize = 100;

/// A running agent: a private directory containing a Unix socket, a set of
/// loaded private keys, and the task accepting connections on it.
///
/// The socket grants signing to anything that can connect to it, so the
/// directory holding it is created with `0700` permissions and a name no
/// other process can predict. Dropping this removes the socket and the
/// directory, and aborts both the accept loop *and* every connection it is
/// still serving (see [`serve`]) — so an agent lives exactly as long as the
/// value representing it, with no window in which an already-connected
/// client can still obtain signatures.
#[derive(Debug)]
pub struct Keyring {
    directory: PathBuf,
    socket_path: PathBuf,
    server: tokio::task::JoinHandle<()>,
}

impl Keyring {
    /// Loads every key in `key_files` and starts serving them.
    ///
    /// Fails if any key can't be read, is protected by a passphrase, or uses
    /// an algorithm this agent can't sign with — all of which are reported
    /// here, up front, rather than as an opaque authentication failure
    /// inside a build minutes later.
    pub async fn start(key_files: &[PathBuf]) -> Result<Self> {
        let keys = load_keys(key_files)?;
        let directory = create_private_directory()?;
        let socket_path = directory.join(SOCKET_FILE_NAME);

        // From here on the directory exists, so every failure has to remove
        // it again — otherwise a bind failure leaks an empty directory into
        // the temporary directory on every attempt.
        let result = check_socket_path_length(&socket_path).and_then(|()| {
            let listener = UnixListener::bind(&socket_path)
                .with_context(|| format!("Failed to listen on '{}'", socket_path.display()))?;
            restrict_socket_to_owner(&socket_path)?;
            Ok(listener)
        });
        let listener = match result {
            Ok(listener) => listener,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&directory);
                return Err(error);
            }
        };

        let server = tokio::spawn(serve(listener, Arc::new(keys)));
        Ok(Self {
            directory,
            socket_path,
            server,
        })
    }

    /// Where this agent is listening — the value to hand to anything
    /// expecting an `SSH_AUTH_SOCK`.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for Keyring {
    fn drop(&mut self) {
        self.server.abort();
        // Best effort: the directory is this process's own, uniquely named,
        // and holds nothing but the socket, so a failure to remove it leaks
        // an empty directory rather than anything meaningful.
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

/// One private key this agent will sign with, alongside the public blob a
/// client identifies it by and the comment `ssh-add -l` displays.
#[derive(Debug)]
struct LoadedKey {
    blob: Vec<u8>,
    comment: String,
    key: PrivateKey,
}

fn load_keys(key_files: &[PathBuf]) -> Result<Vec<LoadedKey>> {
    key_files.iter().map(|path| load_key(path)).collect()
}

fn load_key(path: &Path) -> Result<LoadedKey> {
    let key = PrivateKey::read_openssh_file(path)
        .with_context(|| format!("Failed to read the SSH private key '{}'", path.display()))?;

    if key.is_encrypted() {
        // Go BuildKit's own keyring doesn't handle these either (its
        // `ssh.ParseRawPrivateKey` call carries a `TODO: prompt
        // passphrase?`), so refusing is parity rather than a shortfall —
        // and a build has no terminal to prompt on in any case.
        anyhow::bail!(
            "The SSH private key '{}' is protected by a passphrase, which this agent \
             cannot use — load it into a running ssh-agent and forward that instead, \
             or use a key with no passphrase",
            path.display()
        );
    }

    match key.key_data() {
        KeypairData::Ed25519(_) | KeypairData::Rsa(_) | KeypairData::Ecdsa(_) => {}
        _ => anyhow::bail!(
            "The SSH private key '{}' uses the algorithm '{}', which this agent \
             cannot sign with — Ed25519, RSA and ECDSA keys are supported",
            path.display(),
            key.algorithm()
        ),
    }

    let blob = key
        .public_key()
        .to_bytes()
        .with_context(|| format!("Failed to encode the public key of '{}'", path.display()))?;

    Ok(LoadedKey {
        blob,
        comment: key.comment().to_string(),
        key,
    })
}

/// Creates this agent's own directory under the system temporary directory,
/// readable and traversable only by the current user.
///
/// The name is unpredictable *and* the mode is restrictive, which together
/// are what keep the socket private: the system temporary directory is
/// world-writable, so a predictable name would let another local user
/// pre-create the path, and a permissive mode would let them connect to the
/// socket and sign with these keys for as long as the build runs.
fn create_private_directory() -> Result<PathBuf> {
    // Half a UUID: 48 bits of unpredictability, while keeping the socket
    // path comfortably inside `sun_path` (see `MAX_SOCKET_PATH_LENGTH`).
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let directory = std::env::temp_dir().join(format!("{DIRECTORY_NAME_PREFIX}{}", &suffix[..12]));

    // `create` (not `create_all`) fails rather than reusing an existing
    // directory, so a name collision — or another user having got there
    // first — is an error instead of a silently shared socket directory.
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .with_context(|| format!("Failed to create '{}'", directory.display()))?;

    Ok(directory)
}

/// Narrows the socket file itself to `0600`, so it is the *second*
/// independent barrier rather than relying on its directory alone.
///
/// `bind` creates the socket under the process umask — commonly `022`,
/// which leaves it world-readable at `0755`. The enclosing `0700` directory
/// already makes that unreachable, and this makes it unreachable a second
/// way. OpenSSH's own `ssh-agent` takes exactly this pair of precautions
/// (`mkdtemp` for the directory, `umask(0177)` around the bind for the
/// socket); doing only one of the two leaves the whole protection resting
/// on a single `mode` argument.
///
/// Applied after binding rather than by setting the umask, because the
/// umask is process-global and this process is multi-threaded — a
/// concurrent `create`/`open` anywhere else would silently inherit it. The
/// window where the socket exists at its umask-derived mode is covered by
/// the directory, which is created restrictive before the bind.
fn restrict_socket_to_owner(socket_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("Failed to restrict access to '{}'", socket_path.display()))
}

fn check_socket_path_length(socket_path: &Path) -> Result<()> {
    let length = socket_path.as_os_str().len();
    if length > MAX_SOCKET_PATH_LENGTH {
        anyhow::bail!(
            "The socket path '{}' is {} characters long, which is too long for a Unix \
             socket — set TMPDIR to a shorter directory",
            socket_path.display(),
            length
        );
    }
    Ok(())
}

/// How long to wait before accepting again after an error that isn't a
/// single connection going wrong — long enough that a listener which is
/// genuinely broken can't spin the CPU for the rest of the build, short
/// enough to be invisible if the condition clears.
const ACCEPT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(100);

/// Accepts connections until the task is dropped, serving each one
/// concurrently.
///
/// Connections are spawned into a [`tokio::task::JoinSet`] this task owns,
/// which is what makes the agent stop when [`Keyring`] is dropped: aborting
/// this task drops the set, which aborts every connection still open. A
/// detached `tokio::spawn` per connection would leave clients able to keep
/// requesting signatures after the build that needed them had finished —
/// and would break the guarantee that an agent lives exactly as long as the
/// value representing it.
///
/// Accept errors never end the loop. Most are per-connection
/// (`ECONNABORTED`) or transient resource limits (`EMFILE`), and a build's
/// later `RUN --mount=type=ssh` steps still need an agent — quietly serving
/// nothing for the rest of the build is the worst available outcome, since
/// it surfaces as an unexplained authentication failure much later.
async fn serve(listener: UnixListener, keys: Arc<Vec<LoadedKey>>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let keys = Arc::clone(&keys);
                    connections.spawn(async move {
                        if let Err(error) = serve_connection(stream, &keys).await {
                            tracing::debug!(%error, "ssh-agent connection failed");
                        }
                    });
                }
                Err(error) => {
                    let transient = matches!(
                        error.kind(),
                        ErrorKind::ConnectionAborted | ErrorKind::Interrupted
                    );
                    tracing::debug!(%error, "ssh-agent failed to accept a connection");
                    if !transient {
                        tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                    }
                }
            },
            // Reap finished connections, so the set doesn't grow for the
            // whole life of the agent. Disabled when empty, where
            // `join_next` resolves to `None` immediately.
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

/// Serves one client until it disconnects. Every message is a four-byte
/// big-endian length followed by that many bytes, in both directions (RFC
/// 9987 §4).
async fn serve_connection(mut stream: UnixStream, keys: &[LoadedKey]) -> std::io::Result<()> {
    loop {
        let mut length = [0u8; 4];
        match stream.read_exact(&mut length).await {
            Ok(_) => {}
            // The client hung up between messages, which is how an ordinary
            // session ends rather than a failure.
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        }

        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > MAX_MESSAGE_LENGTH {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                format!("ssh-agent request length {length} is out of range"),
            ));
        }

        let mut request = vec![0u8; length];
        stream.read_exact(&mut request).await?;

        let response = respond(keys, &request);
        stream
            .write_all(&(response.len() as u32).to_be_bytes())
            .await?;
        stream.write_all(&response).await?;
        stream.flush().await?;
    }
}

/// Turns one request message (its type byte plus contents, without the
/// length prefix) into the response message to send back.
///
/// Anything unrecognized, malformed, or truncated becomes
/// `SSH_AGENT_FAILURE` — a client is entitled to send requests this agent
/// doesn't implement, and must get an answer rather than a dropped
/// connection.
fn respond(keys: &[LoadedKey], request: &[u8]) -> Vec<u8> {
    let mut reader = Reader::new(request);
    match reader.u8() {
        // Trailing bytes are ignored: this request has no contents, and a
        // client that sends some is still asking the same question.
        Some(SSH_AGENTC_REQUEST_IDENTITIES) => identities_answer(keys),
        Some(SSH_AGENTC_SIGN_REQUEST) => sign_response(keys, &mut reader).unwrap_or_else(failure),
        _ => failure(),
    }
}

fn failure() -> Vec<u8> {
    vec![SSH_AGENT_FAILURE]
}

fn identities_answer(keys: &[LoadedKey]) -> Vec<u8> {
    let mut response = vec![SSH_AGENT_IDENTITIES_ANSWER];
    response.extend_from_slice(&(keys.len() as u32).to_be_bytes());
    for key in keys {
        write_string(&mut response, &key.blob);
        write_string(&mut response, key.comment.as_bytes());
    }
    response
}

/// `None` for any request that can't be answered — malformed, naming a key
/// this agent doesn't hold, or asking for a signature algorithm it won't
/// produce — which [`respond`] turns into `SSH_AGENT_FAILURE`.
fn sign_response(keys: &[LoadedKey], reader: &mut Reader<'_>) -> Option<Vec<u8>> {
    let blob = reader.string()?;
    let data = reader.string()?;
    let flags = reader.u32()?;

    let key = keys.iter().find(|key| key.blob == blob)?;
    let signature = sign(&key.key, data, flags)?;

    let mut response = vec![SSH_AGENT_SIGN_RESPONSE];
    write_string(&mut response, &signature);
    Some(response)
}

/// Signs `data`, returning the `string algorithm` + `string blob` pair RFC
/// 9987 §5.3 calls the signature — which is exactly what `ssh-key`'s own
/// `Signature` encodes to, so there's no separate wire format to assemble.
fn sign(key: &PrivateKey, data: &[u8], flags: u32) -> Option<Vec<u8>> {
    let signature = match key.key_data() {
        KeypairData::Rsa(keypair) => rsa_signature(keypair, data, flags)?,
        // Every other algorithm has exactly one signature format, and RFC
        // 9987's flags are defined only for RSA, so they're ignored here.
        _ => key.try_sign(data).ok()?,
    };
    Vec::<u8>::try_from(signature).ok()
}

/// Signs with an RSA key under whichever SHA-2 algorithm the client asked
/// for.
///
/// A request with neither flag set is asking for the legacy SHA-1 `ssh-rsa`
/// algorithm, which is refused: OpenSSH has disabled it by default on both
/// ends since 8.8 (2021), so a client still asking for it in an image build
/// is talking to something that would reject the signature anyway. This is a
/// deliberate, narrow divergence from Go BuildKit's keyring, which inherits
/// SHA-1 support from `x/crypto`.
///
/// Signing goes through `try_sign_with_rng`, never `try_sign`: the `rsa`
/// crate applies RSA blinding *only* when it is given a source of
/// randomness, and blinding is the mitigation for the timing side channel
/// [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071)
/// describes. There is no fixed release of `rsa` to upgrade to, so taking
/// the blinded path is the mitigation available — see `audit.toml`.
fn rsa_signature(keypair: &RsaKeypair, data: &[u8], flags: u32) -> Option<Signature> {
    let private_key = rsa_private_key(keypair)?;
    let mut rng = rsa::rand_core::OsRng;

    // A client sets exactly one of these; preferring the stronger one is
    // only a tie-break for a request that sets both.
    let (hash, blob) = if flags & SSH_AGENT_RSA_SHA2_512 != 0 {
        let signing_key = rsa::pkcs1v15::SigningKey::<ssh_key::sha2::Sha512>::new(private_key);
        (
            HashAlg::Sha512,
            signing_key.try_sign_with_rng(&mut rng, data).ok()?.to_vec(),
        )
    } else if flags & SSH_AGENT_RSA_SHA2_256 != 0 {
        let signing_key = rsa::pkcs1v15::SigningKey::<ssh_key::sha2::Sha256>::new(private_key);
        (
            HashAlg::Sha256,
            signing_key.try_sign_with_rng(&mut rng, data).ok()?.to_vec(),
        )
    } else {
        return None;
    };

    Signature::new(Algorithm::Rsa { hash: Some(hash) }, blob).ok()
}

/// Rebuilds the `rsa` crate's own private key from a parsed OpenSSH RSA
/// keypair's components.
///
/// `ssh-key` offers `TryFrom<&RsaKeypair>` for exactly this, and it is
/// **broken in 0.6.7** (the latest release of that line): it passes the
/// prime `p` twice instead of `p` and `q`, so `from_components` rejects
/// every real key and no RSA signature can be produced at all — including
/// through `ssh-key`'s own `Signer` impl, which is why that isn't used
/// either. Fixed only on the unreleased 0.7 line, so this stays until
/// `ssh-key` 0.7 is published and adopted.
///
/// `from_components` validates the result (that `n` really is `p * q`, and
/// that `d` is consistent with `e`), so a key whose components don't agree
/// is rejected here rather than producing signatures that never verify.
fn rsa_private_key(keypair: &RsaKeypair) -> Option<rsa::RsaPrivateKey> {
    let component =
        |mpint: &ssh_key::Mpint| mpint.as_positive_bytes().map(rsa::BigUint::from_bytes_be);

    rsa::RsaPrivateKey::from_components(
        component(&keypair.public.n)?,
        component(&keypair.public.e)?,
        component(&keypair.private.d)?,
        vec![
            component(&keypair.private.p)?,
            component(&keypair.private.q)?,
        ],
    )
    .ok()
}

/// Reads the primitives RFC 9987 messages are built from: `byte`, `uint32`
/// (big-endian), and `string` (a `uint32` length followed by that many
/// bytes). Every read is bounds-checked and yields `None` on a truncated or
/// malformed message, so no request can panic the agent.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(count)?;
        let slice = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|bytes| bytes[0])
    }

    fn u32(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(u32::from_be_bytes(bytes))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let length = self.u32()?;
        self.take(length as usize)
    }
}

fn write_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

#[cfg(test)]
#[path = "ssh_agent_tests.rs"]
mod tests;
