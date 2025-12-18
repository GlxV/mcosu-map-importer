use std::io::BufReader;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink};

use crate::app_state::AudioPreviewStatus;

#[derive(Clone)]
pub struct AudioPlayer {
    inner: Arc<Mutex<PlayerState>>,
}

struct PlayerState {
    stream: Option<OutputStream>,
    handle: Option<OutputStreamHandle>,
    current: Option<Current>,
}

struct Current {
    entry_id: u64,
    sink: Sink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackAction {
    StartFresh,
    StopThenStart,
    PauseCurrent,
    ResumeCurrent,
}

pub(crate) fn decide_playback_action(
    current: Option<(u64, bool)>,
    request_id: u64,
) -> PlaybackAction {
    match current {
        Some((id, paused)) if id == request_id => {
            if paused {
                PlaybackAction::ResumeCurrent
            } else {
                PlaybackAction::PauseCurrent
            }
        }
        Some(_) => PlaybackAction::StopThenStart,
        None => PlaybackAction::StartFresh,
    }
}

impl AudioPlayer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PlayerState {
                stream: None,
                handle: None,
                current: None,
            })),
        }
    }

    pub fn toggle(&self, entry_id: u64, path: &Path) -> Result<AudioPreviewStatus> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|e| anyhow::anyhow!("audio lock poisoned: {e}"))?;
        guard.ensure_stream()?;

        let action = decide_playback_action(
            guard
                .current
                .as_ref()
                .map(|c| (c.entry_id, c.sink.is_paused())),
            entry_id,
        );
        if matches!(action, PlaybackAction::PauseCurrent | PlaybackAction::ResumeCurrent) {
            if let Some(current) = guard.current.as_mut() {
                match action {
                    PlaybackAction::PauseCurrent => {
                        current.sink.pause();
                        return Ok(AudioPreviewStatus::Paused);
                    }
                    PlaybackAction::ResumeCurrent => {
                        current.sink.play();
                        return Ok(AudioPreviewStatus::Playing);
                    }
                    _ => {}
                }
            }
        }
        if matches!(action, PlaybackAction::StopThenStart) {
            if let Some(current) = guard.current.take() {
                current.sink.stop();
            }
        }

        let handle = guard
            .handle
            .as_ref()
            .context("saida de audio indisponivel")?
            .clone();
        let file = std::fs::File::open(path).with_context(|| format!("abrindo audio {:?}", path))?;
        let sink = Sink::try_new(&handle).context("criando sink de audio")?;
        let source = Decoder::new(BufReader::new(file)).context("decodificando audio")?;
        sink.append(source);
        sink.play();

        guard.current = Some(Current {
            entry_id,
            sink,
        });
        Ok(AudioPreviewStatus::Playing)
    }
}

impl PlayerState {
    fn ensure_stream(&mut self) -> Result<()> {
        if self.stream.is_none() || self.handle.is_none() {
            let (stream, handle) = OutputStream::try_default().context("nenhum dispositivo de audio encontrado")?;
            self.stream = Some(stream);
            self.handle = Some(handle);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_playback_action_handles_singleton() {
        assert_eq!(decide_playback_action(None, 1), PlaybackAction::StartFresh);
        assert_eq!(
            decide_playback_action(Some((1, false)), 1),
            PlaybackAction::PauseCurrent
        );
        assert_eq!(
            decide_playback_action(Some((1, true)), 1),
            PlaybackAction::ResumeCurrent
        );
        assert_eq!(
            decide_playback_action(Some((2, false)), 1),
            PlaybackAction::StopThenStart
        );
    }
}
