//! Shared protocol loop for paster modules.
//!
//! A paster owns a backend capable of replaying its paste chord and speaks
//! the NDJSON paster protocol from [`cliphistory_proto`]: `Ready` on start,
//! `Pong` for `Ping`, chord playback for `Paste`, clean exit on `Stop`.

use anyhow::Result;
use cliphistory_proto::{HostToPaster, PasterToHost, PROTOCOL_VERSION};

use crate::{emit, next_frame};

/// Anything that can replay the paste chord on demand.
pub trait PasteBackend {
    /// Inject the chord into whatever surface holds keyboard focus.
    fn play(&mut self) -> Result<()>;
}

/// Human-readable chord description for debug logs.
pub type BackendName = &'static str;

/// Run the paster protocol over stdio until `Stop` or EOF.
///
/// `backend_name` only feeds log lines. Playback failures are reported to
/// the host as `Error` frames; the loop keeps running so one bad paste
/// never kills the module.
pub fn serve<B: PasteBackend>(backend_name: BackendName, mut backend: B) -> Result<()> {
    emit(&PasterToHost::Ready {
        protocol_version: PROTOCOL_VERSION,
    })?;

    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    while let Some(frame) =
        next_frame::<HostToPaster, _>(&mut reader, |line, e| {
            let _ = emit(&PasterToHost::Error {
                message: format!("unparsable frame {e}: {line:.120}"),
            });
        })
    {
        match frame {
            HostToPaster::Ping => emit(&PasterToHost::Pong)?,
            HostToPaster::Paste => {
                let t = std::time::Instant::now();
                let result = backend.play();
                log::info!(
                    "paste chord via {backend_name} injected in {:?}",
                    t.elapsed()
                );
                if let Err(e) = result {
                    let _ = emit(&PasterToHost::Error {
                        message: format!("paste failed: {e:#}"),
                    });
                }
            }
            HostToPaster::Stop => return Ok(()),
        }
    }
    // Host closed stdin: treat as shutdown.
    Ok(())
}
