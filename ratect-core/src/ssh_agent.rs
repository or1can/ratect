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
mod tests {
    use super::*;

    use rand_chacha::rand_core::SeedableRng;
    use signature::Verifier;
    use ssh_key::EcdsaCurve;

    /// Every request below is assembled from the numbers written in [RFC
    /// 9987](https://www.rfc-editor.org/rfc/rfc9987) rather than from this
    /// module's own constants, and every response is checked the same way.
    /// A test that derives its input from the code under test can't tell a
    /// correct constant from a wrong one — both sides would move together.
    const REQUEST_IDENTITIES: u8 = 11;
    const IDENTITIES_ANSWER: u8 = 12;
    const SIGN_REQUEST: u8 = 13;
    const SIGN_RESPONSE: u8 = 14;
    const FAILURE: u8 = 5;
    const RSA_SHA2_256_FLAG: u32 = 2;
    const RSA_SHA2_512_FLAG: u32 = 4;

    /// A deterministic key, so a failure is always the same failure. The
    /// seed carries no meaning beyond being fixed.
    fn test_key(seed: u8, comment: &str) -> LoadedKey {
        let mut rng = rand_chacha::ChaCha20Rng::from_seed([seed; 32]);
        let mut key = PrivateKey::random(&mut rng, Algorithm::Ed25519).unwrap();
        key.set_comment(comment);
        LoadedKey {
            blob: key.public_key().to_bytes().unwrap(),
            comment: comment.to_string(),
            key,
        }
    }

    /// A deterministic key on the given curve — same reasoning as
    /// [`test_key`], and fast enough to generate per test unlike RSA below.
    fn test_ecdsa_key(curve: EcdsaCurve) -> LoadedKey {
        let mut rng = rand_chacha::ChaCha20Rng::from_seed([7; 32]);
        let key = PrivateKey::random(&mut rng, Algorithm::Ecdsa { curve }).unwrap();
        LoadedKey {
            blob: key.public_key().to_bytes().unwrap(),
            comment: key.comment().to_string(),
            key,
        }
    }

    /// A throwaway 2048-bit RSA key, generated by `ssh-keygen` for these
    /// tests alone and never used to authenticate to anything. Embedded
    /// rather than generated: RSA key generation takes tens of seconds in an
    /// unoptimized test build, where an Ed25519 key is instant.
    const TEST_RSA_KEY: &str = "\
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABFwAAAAdzc2gtcn
NhAAAAAwEAAQAAAQEA2Hy24J/ZN803X2Yy5mg3+eHVD4XhPcvjDa7fTpkndBFGl4g/+0es
NPNTdGJIJ3CSlVOMfChfIzlvYy8G8Dd7UlMT+pgxMQ0fUtaTVZ5eI71YIW3WEV9leDR9TZ
NPVFpzEvihoXdiSQpNGzD3xd4lJVYXEAKt9nYo/uTIcFrPWDVLQQwbLRVeJ1pysCS5Iabr
Jzk11f+u+C2bFeyYaVx00jc33x5HIihjJZkSs6nXOVMRhPX6x+LynrWJkH/iRztsgZJk49
7JcqH80+w8ZqYGOs1qIaiDH62NznhJ9LUND235Enr0cIeYk6jRnjbGy7Hn3m54+Q5z+wK2
89FTCHkVyQAAA8j1uSZ39bkmdwAAAAdzc2gtcnNhAAABAQDYfLbgn9k3zTdfZjLmaDf54d
UPheE9y+MNrt9OmSd0EUaXiD/7R6w081N0YkgncJKVU4x8KF8jOW9jLwbwN3tSUxP6mDEx
DR9S1pNVnl4jvVghbdYRX2V4NH1Nk09UWnMS+KGhd2JJCk0bMPfF3iUlVhcQAq32dij+5M
hwWs9YNUtBDBstFV4nWnKwJLkhpusnOTXV/674LZsV7JhpXHTSNzffHkciKGMlmRKzqdc5
UxGE9frH4vKetYmQf+JHO2yBkmTj3slyofzT7DxmpgY6zWohqIMfrY3OeEn0tQ0PbfkSev
Rwh5iTqNGeNsbLsefebnj5DnP7Arbz0VMIeRXJAAAAAwEAAQAAAQBBS8SBFdWXnh8geBvM
IQ0ZIoN37iKU2AVA4EjcVRdwS7GmDON3cBPB2M6IIQiwVKTxw0RxQmAHqNAu80U9eQ6KMy
Koh/T2XYXgH3ZK8bxlPTvywUU68jIRwos0tcTMpYdl5nYh1HdhnmjJVci19p3vl/rWymgc
GWGeF/VY5hr5+HMD/+AKRnD1usH7QOtqBa2LBmfEcG4a3c3Hp7F9euENtYaaiT9C4d1RwS
M481Z1d9rwillV8kAUxA1Xg4ltDI9h9rTF4/IU16Iq8GgM+UkVi19ZGwwTZrUKlP+A1ksP
GMjjbKz9GbLDP3gqFw/8RJ7KevLnSW4xD7i11n5pHi8dAAAAgAVQzJMUZEhd6Akts3sjaF
qKDFreHoTtjI61ekTHg+HSYT48xx+KWmXXREO9QBOTOqaOj8wiMKeUYw/w3Dsd5BEpXOhr
TZ7YiLceOnFG3+XGyQ9DZfsn8dPTLxihsEGYbfNKJuPmNllMDlGJgdqjRIDhZ5eFyWRdB8
WwMl6bj8BUAAAAgQDzwsJbLMbZDiKkFvqF7KcyKF9glPYrcR1fzoDZUJJoNfEaSKhCjtGR
gxI/9ccbF8Gdf9fmIj1KZnb03dZS60Mdjxa/UsoGuuRNf5rHe8ZO8pLyENkd3gKr1F2qtZ
LMJY54dqnMHvZ3db9jUSb2uS9j3Ij3TnB1q70iKR25KK6qSwAAAIEA41tjCuTozPlCRmtv
w3Rka90qlZ2OQNOko+c3M+/56/d0GZ+8W1Gf79qs/V/4u64r+lDChQFK+YZtW6en2ew+L4
woLocI7INRyn4uUjRGbbkuFuBHMFVbDDtgO18gBwFQ8HZySXZ7IKoM30BXUEgeXaH1s/2T
fzqxqMSMGQFJc7sAAAAMcnNhLXRlc3Qta2V5AQIDBAUGBw==
-----END OPENSSH PRIVATE KEY-----
";

    fn test_rsa_key() -> LoadedKey {
        let key = PrivateKey::from_openssh(TEST_RSA_KEY).unwrap();
        LoadedKey {
            blob: key.public_key().to_bytes().unwrap(),
            comment: key.comment().to_string(),
            key,
        }
    }

    fn sign_request(blob: &[u8], data: &[u8], flags: u32) -> Vec<u8> {
        let mut request = vec![SIGN_REQUEST];
        write_string(&mut request, blob);
        write_string(&mut request, data);
        request.extend_from_slice(&flags.to_be_bytes());
        request
    }

    /// Unwraps a `SSH_AGENT_SIGN_RESPONSE`, returning the signature's
    /// algorithm name and its raw blob — the `string algorithm` +
    /// `string blob` pair RFC 9987 §5.3 defines.
    fn signature_parts(response: &[u8]) -> (String, Vec<u8>) {
        assert_eq!(response[0], SIGN_RESPONSE, "not a sign response");
        let mut reader = Reader::new(&response[1..]);
        let signature = reader.string().expect("truncated signature");
        let mut inner = Reader::new(signature);
        let algorithm = inner.string().expect("no signature algorithm");
        let blob = inner.string().expect("no signature blob");
        (
            String::from_utf8(algorithm.to_vec()).unwrap(),
            blob.to_vec(),
        )
    }

    /// The identities answer's exact wire layout: the message number, a
    /// `uint32` key count, then one `string` public blob and `string`
    /// comment per key, in order.
    #[test]
    fn listing_identities_returns_every_key_with_its_comment() {
        let keys = vec![test_key(1, "first-key"), test_key(2, "second-key")];

        let response = respond(&keys, &[REQUEST_IDENTITIES]);

        assert_eq!(response[0], IDENTITIES_ANSWER);
        let mut reader = Reader::new(&response[1..]);
        assert_eq!(reader.u32(), Some(2));
        for key in &keys {
            assert_eq!(reader.string(), Some(key.blob.as_slice()));
            assert_eq!(reader.string(), Some(key.comment.as_bytes()));
        }
        assert_eq!(reader.take(1), None, "unexpected trailing bytes");
    }

    /// An agent holding nothing still answers, rather than failing — that's
    /// how a client learns there is nothing to try.
    #[test]
    fn listing_identities_with_no_keys_answers_with_a_count_of_zero() {
        let response = respond(&[], &[REQUEST_IDENTITIES]);

        assert_eq!(response, vec![IDENTITIES_ANSWER, 0, 0, 0, 0]);
    }

    /// The signature has to verify against the *public* key, which is the
    /// only check that would catch signing the wrong bytes, hashing them
    /// differently, or mislabelling the algorithm.
    #[test]
    fn signing_produces_a_signature_the_public_key_verifies() {
        let keys = vec![test_key(3, "signing-key")];
        let data = b"session identifier and authentication request";

        let response = respond(&keys, &sign_request(&keys[0].blob, data, 0));

        let (algorithm, _) = signature_parts(&response);
        assert_eq!(algorithm, "ssh-ed25519");
        let mut reader = Reader::new(&response[1..]);
        let signature = Signature::try_from(reader.string().unwrap()).unwrap();
        // Fully qualified: `PublicKey` has an inherent `verify` for `sshsig`
        // signatures that would otherwise shadow the trait method.
        Verifier::verify(keys[0].key.public_key(), data, &signature)
            .expect("the signature should verify against the public key");
    }

    /// The ECDSA curves are enabled as `ssh-key` features and accepted by
    /// [`load_keys`], so each one needs a signature that actually verifies —
    /// the features cost dependencies, and "it compiles" is not evidence the
    /// path works. Signing goes through `try_sign` here rather than
    /// [`rsa_signature`], and RFC 9987's flags are RSA-only, so a client
    /// sending flags with an ECDSA key must still get its one valid
    /// signature format back rather than a failure.
    #[test]
    fn signing_with_an_ecdsa_key_verifies_on_every_supported_curve() {
        for (curve, expected) in [
            (EcdsaCurve::NistP256, "ecdsa-sha2-nistp256"),
            (EcdsaCurve::NistP384, "ecdsa-sha2-nistp384"),
            (EcdsaCurve::NistP521, "ecdsa-sha2-nistp521"),
        ] {
            let keys = vec![test_ecdsa_key(curve)];
            let data = b"session identifier and authentication request";

            let response = respond(&keys, &sign_request(&keys[0].blob, data, RSA_SHA2_512_FLAG));

            let (algorithm, _) = signature_parts(&response);
            assert_eq!(algorithm, expected);
            let mut reader = Reader::new(&response[1..]);
            let signature = Signature::try_from(reader.string().unwrap()).unwrap();
            Verifier::verify(keys[0].key.public_key(), data, &signature)
                .unwrap_or_else(|_| panic!("the {expected} signature should verify"));
        }
    }

    /// RFC 9987's flags exist for RSA alone, and getting them wrong is
    /// invisible until a real server rejects the signature: the algorithm
    /// name in the response has to match the hash actually used, so both
    /// halves are checked.
    #[test]
    fn signing_with_an_rsa_key_honours_the_requested_sha2_algorithm() {
        let keys = vec![test_rsa_key()];
        let data = b"session identifier and authentication request";

        for (flags, expected) in [
            (RSA_SHA2_512_FLAG, "rsa-sha2-512"),
            (RSA_SHA2_256_FLAG, "rsa-sha2-256"),
        ] {
            let response = respond(&keys, &sign_request(&keys[0].blob, data, flags));

            let (algorithm, _) = signature_parts(&response);
            assert_eq!(algorithm, expected, "for flags {flags}");
            let mut reader = Reader::new(&response[1..]);
            let signature = Signature::try_from(reader.string().unwrap()).unwrap();
            Verifier::verify(keys[0].key.public_key(), data, &signature)
                .unwrap_or_else(|_| panic!("the {expected} signature should verify"));
        }
    }

    /// No flags means the legacy SHA-1 `ssh-rsa` algorithm, which this
    /// agent refuses — see [`rsa_signature`]. Answering with a SHA-2
    /// signature instead would be worse than failing: the client asked for
    /// one algorithm and would be told it got another.
    #[test]
    fn signing_with_an_rsa_key_refuses_the_legacy_sha1_algorithm() {
        let keys = vec![test_rsa_key()];

        let response = respond(&keys, &sign_request(&keys[0].blob, b"data", 0));

        assert_eq!(response, vec![FAILURE]);
    }

    /// A client picks the key to sign with by its public blob, so a blob
    /// this agent doesn't hold must fail rather than being served by
    /// whichever key happens to be first.
    #[test]
    fn signing_with_an_unknown_key_fails() {
        let keys = vec![test_key(4, "held-key")];
        let other = test_key(5, "not-held-key");

        let response = respond(&keys, &sign_request(&other.blob, b"data", 0));

        assert_eq!(response, vec![FAILURE]);
    }

    /// A truncated request must be answered, not crash the agent or hang
    /// the connection — `Reader` bounds-checks every read for this reason.
    #[test]
    fn a_truncated_sign_request_fails_rather_than_panicking() {
        let keys = vec![test_key(6, "key")];
        let full = sign_request(&keys[0].blob, b"data", 0);

        for length in 0..full.len() {
            assert_eq!(
                respond(&keys, &full[..length]),
                vec![FAILURE],
                "a {length}-byte prefix of a sign request should fail"
            );
        }
    }

    /// Writes `key` out in OpenSSH's own private key format, so `load_key`
    /// is exercised against a real file rather than an in-memory value.
    fn write_key_file(directory: &Path, name: &str, key: &PrivateKey) -> PathBuf {
        std::fs::create_dir_all(directory).unwrap();
        let path = directory.join(name);
        std::fs::write(
            path.as_path(),
            key.to_openssh(ssh_key::LineEnding::LF).unwrap(),
        )
        .unwrap();
        path
    }

    fn unique_temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("ratect-ssh-agent-test-{}", uuid::Uuid::new_v4()))
    }

    /// The comment is what `ssh-add -l` shows inside the build, so it has
    /// to survive the round trip through the file rather than being
    /// replaced by the file's name or left empty.
    #[test]
    fn loading_a_key_file_keeps_its_public_blob_and_comment() {
        let directory = unique_temp_dir();
        let key = test_key(8, "loaded-from-a-file").key;
        let path = write_key_file(&directory, "id_ed25519", &key);

        let loaded = load_keys(&[path]).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].comment, "loaded-from-a-file");
        assert_eq!(loaded[0].blob, key.public_key().to_bytes().unwrap());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// A throwaway Ed25519 key protected by the passphrase
    /// `test-passphrase`, generated by `ssh-keygen` for this test alone.
    /// Embedded because it can't be produced from within the test: writing
    /// an encrypted key needs `ssh-key`'s `encryption` feature, which is
    /// deliberately off (it alone would pull in ~16 crates for a case this
    /// agent refuses anyway).
    const TEST_ENCRYPTED_KEY: &str = "\
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABBxmy6fV3
HZgz7Icu2B7IjIAAAAGAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIMx6KPDUuTS6adHK
OpqS+OA2lOuWVU/GJZ5lc2DYVdalAAAAoMkFaWwGgNBB6/IprP+RAEYdk/bvEAQcovvVwF
3bR6Fdw1/wm9O9oDi73gRFDg9Fk0uHP0X4REspg2m4UcPdq84Ca7TVaV78xC7+db4YY3Ec
10Cc3OG3w2f3VkIVIFdgPW6iMy88u5jP2aLqHnKuJLjhqcvWE1lnhMWvG0baB58PWRHe4s
wq5n6O9bZ1kNQ1vKBt7358x8sWgvoxUAOLDF0=
-----END OPENSSH PRIVATE KEY-----
";

    /// There is no terminal to prompt on during a build, so a
    /// passphrase-protected key has to be refused *at load*, naming the
    /// file. Left to signing time it would surface as an unexplained
    /// authentication failure deep inside someone's Dockerfile.
    #[test]
    fn loading_a_passphrase_protected_key_is_refused_and_names_the_file() {
        let directory = unique_temp_dir();
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("id_ed25519");
        std::fs::write(&path, TEST_ENCRYPTED_KEY).unwrap();

        let error = load_keys(std::slice::from_ref(&path)).unwrap_err();

        let message = format!("{error:#}");
        assert!(
            message.contains("passphrase"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains(&path.display().to_string()),
            "the error should name the file, but was: {message}"
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// A missing or unreadable key has to name the file — this is the error
    /// a user sees when a `paths` entry points at the wrong place, and
    /// "failed to read a key" would leave them guessing which one.
    #[test]
    fn loading_a_missing_key_file_names_it() {
        let path = unique_temp_dir().join("absent_key");

        let error = load_keys(std::slice::from_ref(&path)).unwrap_err();

        assert!(
            format!("{error}").contains(&path.display().to_string()),
            "the error should name the missing file, but was: {error}"
        );
    }

    /// The whole path, not just the directory, has to fit `sun_path` — so
    /// the check has to include the socket's own file name.
    #[test]
    fn a_socket_path_longer_than_the_unix_limit_is_rejected() {
        let long = PathBuf::from("/tmp").join("d".repeat(120)).join("agent");

        let error = check_socket_path_length(&long).unwrap_err();

        assert!(
            format!("{error}").contains("too long"),
            "unexpected error: {error}"
        );
        check_socket_path_length(Path::new("/tmp/ratect-ssh-0123456789ab/agent"))
            .expect("a realistic socket path should be accepted");
    }

    /// Add, remove, lock, unlock, smartcard and extension requests all land
    /// here: this agent implements none of them, and a client is entitled
    /// to an answer rather than a dropped connection.
    #[test]
    fn an_unimplemented_request_fails() {
        let keys = vec![test_key(7, "key")];

        // 17 is SSH_AGENTC_ADD_IDENTITY; 25 is SSH_AGENTC_EXTENSION.
        for request_type in [17u8, 18, 22, 23, 25] {
            assert_eq!(respond(&keys, &[request_type]), vec![FAILURE]);
        }
        assert_eq!(respond(&keys, &[]), vec![FAILURE], "empty request");
    }

    /// Sends one length-prefixed request and reads the length-prefixed
    /// response back, the way BuildKit's forwarded stream will.
    async fn round_trip(stream: &mut UnixStream, request: &[u8]) -> Vec<u8> {
        stream
            .write_all(&(request.len() as u32).to_be_bytes())
            .await
            .unwrap();
        stream.write_all(request).await.unwrap();

        let mut length = [0u8; 4];
        stream.read_exact(&mut length).await.unwrap();
        let mut response = vec![0u8; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut response).await.unwrap();
        response
    }

    /// The four-byte length prefix in each direction, the connection's own
    /// request loop, the directory's permissions and the cleanup on drop
    /// are only exercised by a real connection over a real socket — so this
    /// drives one, rather than calling [`respond`] directly as the tests
    /// above do.
    #[tokio::test]
    async fn a_client_can_talk_to_the_agent_over_its_socket() {
        use std::os::unix::fs::PermissionsExt;

        let directory = unique_temp_dir();
        let key = test_key(10, "over-the-socket").key;
        let key_file = write_key_file(&directory, "id_ed25519", &key);

        let keyring = Keyring::start(&[key_file]).await.unwrap();
        let socket_directory = keyring.socket_path().parent().unwrap().to_path_buf();

        // The socket grants signing to anything that can reach it, so the
        // directory holding it must not be traversable by other users.
        let mode = std::fs::metadata(&socket_directory)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "the agent's directory should be private to this user"
        );

        // The second, independent barrier. `bind` leaves the socket at the
        // process umask (commonly `0755`), which the directory above
        // already covers — but resting the whole protection on one `mode`
        // argument is what this guards against, and it is what OpenSSH's
        // own agent does too.
        let socket_mode = std::fs::metadata(keyring.socket_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            socket_mode & 0o777,
            0o600,
            "the socket itself should be private to this user"
        );

        let mut stream = UnixStream::connect(keyring.socket_path()).await.unwrap();

        let response = round_trip(&mut stream, &[REQUEST_IDENTITIES]).await;
        assert_eq!(response[0], IDENTITIES_ANSWER);
        let mut reader = Reader::new(&response[1..]);
        assert_eq!(reader.u32(), Some(1));
        assert_eq!(
            reader.string(),
            Some(key.public_key().to_bytes().unwrap().as_slice())
        );
        assert_eq!(reader.string(), Some(b"over-the-socket".as_slice()));

        // A client sends many requests down one connection, so the second
        // has to be answered as readily as the first.
        let blob = key.public_key().to_bytes().unwrap();
        let response = round_trip(&mut stream, &sign_request(&blob, b"data", 0)).await;
        assert_eq!(response[0], SIGN_RESPONSE);

        drop(keyring);
        assert!(
            !socket_directory.exists(),
            "dropping the agent should remove its socket directory"
        );

        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// Dropping the agent has to stop the connections it is *already*
    /// serving, not just the accept loop. A detached task per connection
    /// leaves an open client able to keep getting signatures after the
    /// build that needed the keys has finished — the socket file is gone,
    /// but an established connection doesn't care.
    #[tokio::test]
    async fn dropping_the_agent_stops_a_connection_it_is_already_serving() {
        let directory = unique_temp_dir();
        let key = test_key(11, "still-connected").key;
        let key_file = write_key_file(&directory, "id_ed25519", &key);

        let keyring = Keyring::start(&[key_file]).await.unwrap();
        let mut stream = UnixStream::connect(keyring.socket_path()).await.unwrap();

        // Establish the connection properly first: a client that has never
        // been served would prove nothing about tearing one down.
        let response = round_trip(&mut stream, &[REQUEST_IDENTITIES]).await;
        assert_eq!(response[0], IDENTITIES_ANSWER);

        drop(keyring);

        // The abort has to land before the peer is observably gone.
        tokio::task::yield_now().await;
        let mut length = [0u8; 4];
        stream
            .write_all(&[0, 0, 0, 1, REQUEST_IDENTITIES])
            .await
            .ok();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stream.read_exact(&mut length),
        )
        .await
        .expect("the dropped agent should not leave the client waiting");

        assert!(
            read.is_err(),
            "the connection should be closed, but it answered {length:?}"
        );

        std::fs::remove_dir_all(&directory).unwrap();
    }
}
