//! Native communications capture, following Microsoft's AcousticEchoCancellation
//! sample. CPAL remains the fallback when the endpoint has no active AEC APO.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, ensure};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_FAILED};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::AUDIO_EFFECT_TYPE_ACOUSTIC_ECHO_CANCELLATION;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
    WaitForSingleObject,
};
use windows::core::{PCWSTR, w};

use super::super::{Capture, VOICE_RATE};
use super::ECHO_STATUS;

pub struct AecCapture {
    stop: Arc<AtomicBool>,
    corked: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AecCapture {
    pub fn start(
        devices: (Option<String>, Option<String>),
        capture: Arc<Mutex<Capture>>,
        corked: bool,
    ) -> Result<Self, String> {
        let mut stream = Self {
            stop: Arc::new(AtomicBool::new(false)),
            corked: Arc::new(AtomicBool::new(corked)),
            thread: None,
        };
        let (stop, corked) = (Arc::clone(&stream.stop), Arc::clone(&stream.corked));
        let (tx, rx) = mpsc::sync_channel(1);
        stream.thread = Some(
            std::thread::Builder::new()
                .name("fastdiscord-windows-aec".into())
                .spawn(move || {
                    if let Err(error) = run(devices, capture, stop, corked, &tx) {
                        let reason = format!("{error:#}");
                        let _ = tx.try_send(Err(reason.clone()));
                        *ECHO_STATUS.lock().unwrap() = Some(format!(
                            "Cancelamento de eco indisponível: {reason}. Microfone sem cancelamento."
                        ));
                        log::warn!("AEC Windows: {reason}");
                    }
                })
                .map_err(|error| error.to_string())?,
        );
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())??;
        Ok(stream)
    }

    pub fn set_corked(&self, corked: bool) {
        self.corked.store(corked, Ordering::Relaxed);
    }

    pub fn finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
    }
}

impl Drop for AecCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Com;
impl Drop for Com {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct Event(HANDLE);
impl Drop for Event {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct AudioPriority(HANDLE);
impl Drop for AudioPriority {
    fn drop(&mut self) {
        let _ = unsafe { AvRevertMmThreadCharacteristics(self.0) };
    }
}

struct Client(IAudioClient2);
impl Drop for Client {
    fn drop(&mut self) {
        let _ = unsafe { self.0.Stop() };
    }
}

fn endpoint(
    enumerator: &IMMDeviceEnumerator,
    id: &Option<String>,
    flow: EDataFlow,
) -> anyhow::Result<IMMDevice> {
    // DeviceId stores the WASAPI endpoint behind its platform prefix.
    unsafe {
        match id {
            Some(id) => {
                let id: cpal::DeviceId = id.parse()?;
                let wide: Vec<_> = id.id().encode_utf16().chain(Some(0)).collect();
                Ok(enumerator.GetDevice(PCWSTR(wide.as_ptr()))?)
            }
            None => Ok(enumerator.GetDefaultAudioEndpoint(flow, eConsole)?),
        }
    }
}

fn run(
    (output, input): (Option<String>, Option<String>),
    capture: Arc<Mutex<Capture>>,
    stop: Arc<AtomicBool>,
    corked: Arc<AtomicBool>,
    ready: &mpsc::SyncSender<Result<(), String>>,
) -> anyhow::Result<()> {
    // COM objects, WASAPI buffers and event handles stay on this worker.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let _com = Com;
        let event = Event(CreateEventW(None, false, false, None)?);
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = endpoint(&enumerator, &input, eCapture)?;
        let client = Client(device.Activate::<IAudioClient2>(CLSCTX_ALL, None)?);
        client.0.SetClientProperties(&AudioClientProperties {
            cbSize: size_of::<AudioClientProperties>() as u32,
            eCategory: AudioCategory_Communications,
            ..Default::default()
        })?;
        let format = WAVEFORMATEX {
            wFormatTag: 3, // IEEE float.
            nChannels: 1,
            nSamplesPerSec: VOICE_RATE,
            nAvgBytesPerSec: VOICE_RATE * 4,
            nBlockAlign: 4,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        client.0.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            200_000,
            0,
            &format,
            None,
        )?;
        client.0.SetEventHandle(event.0)?;
        let effects: IAudioEffectsManager = client
            .0
            .GetService()
            .context("o Windows/dispositivo não fornece informações de AEC")?;
        let (mut list, mut count) = (std::ptr::null_mut(), 0);
        effects.GetAudioEffects(&mut list, &mut count)?;
        let effect = if count > 0 && !list.is_null() {
            std::slice::from_raw_parts(list, count as usize)
                .iter()
                .find(|effect| effect.id == AUDIO_EFFECT_TYPE_ACOUSTIC_ECHO_CANCELLATION)
                .copied()
        } else {
            None
        };
        CoTaskMemFree(Some(list.cast()));
        let effect = effect.context("este microfone não oferece cancelamento de eco nativo")?;
        if effect.state != AUDIO_EFFECT_STATE_ON && effect.canSetState.as_bool() {
            effects.SetAudioEffectState(effect.id, AUDIO_EFFECT_STATE_ON)?;
        } else {
            ensure!(
                effect.state == AUDIO_EFFECT_STATE_ON,
                "cancelamento de eco desativado pelo dispositivo"
            );
        }
        let reference = endpoint(&enumerator, &output, eRender)?;
        let id = reference.GetId()?;
        let control: windows::core::Result<IAcousticEchoCancellationControl> =
            client.0.GetService();
        let result = match control {
            Ok(control) => control.SetEchoCancellationRenderEndpoint(PCWSTR(id.0)),
            Err(error) => Err(error),
        };
        CoTaskMemFree(Some(id.0.cast()));
        result.context("não foi possível usar a saída escolhida como referência do eco")?;
        let recording: IAudioCaptureClient = client.0.GetService()?;
        let mut playing = !corked.load(Ordering::Relaxed);
        if playing {
            client.0.Start()?;
        }
        *ECHO_STATUS.lock().unwrap() = Some("Cancelamento de eco do Windows ativo.".into());
        let _ = ready.send(Ok(()));
        let mut task_index = 0;
        let _priority = AvSetMmThreadCharacteristicsW(w!("Audio"), &mut task_index)
            .map(AudioPriority)
            .inspect_err(|error| log::warn!("AEC Windows: prioridade de áudio: {error}"))
            .ok();
        let mut silence = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            let want = !corked.load(Ordering::Relaxed);
            if want != playing {
                if want {
                    client.0.Start()?;
                } else {
                    client.0.Stop()?;
                }
                playing = want;
            }
            ensure!(
                WaitForSingleObject(event.0, 50) != WAIT_FAILED,
                "evento de captura falhou"
            );
            if !playing {
                continue;
            }
            while recording.GetNextPacketSize()? > 0 {
                let (mut data, mut frames, mut flags) = (std::ptr::null_mut(), 0, 0);
                recording.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let samples = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null()
                {
                    silence.resize(frames as usize, 0.0);
                    &silence[..frames as usize]
                } else {
                    std::slice::from_raw_parts(data.cast::<f32>(), frames as usize)
                };
                if let Ok(mut capture) = capture.lock() {
                    capture.feed(samples);
                }
                recording.ReleaseBuffer(frames)?;
            }
        }
    }
    Ok(())
}
