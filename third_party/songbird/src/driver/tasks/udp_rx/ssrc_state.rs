use super::*;
use crate::{
    driver::{
        tasks::error::{Error, Result},
        Channels, DecodeConfig, DecodeMode,
    },
    events::context_data::{RtpData, VoiceData},
};
use discortp::{rtp::RtpExtensionPacket, Packet, PacketSize};
use opus2::{Decoder as OpusDecoder, ErrorCode};

#[derive(Debug)]
pub struct SsrcState {
    playout_buffer: PlayoutBuffer,
    crypto_mode: CryptoMode,
    decoder: OpusDecoder,
    decode_size: PacketDecodeSize,
    pub(crate) prune_time: Instant,
    pub(crate) disconnected: bool,
    channels: Channels,
}

impl SsrcState {
    pub fn new(pkt: &RtpPacket<'_>, crypto_mode: CryptoMode, config: &Config) -> Self {
        let playout_capacity = config.playout_buffer_length.get() + config.playout_spike_length;
        let (sample_rate, channels) = match config.decode_mode {
            DecodeMode::Decode(config) => (config.sample_rate, config.channels),
            DecodeMode::Decrypt | DecodeMode::Pass => Default::default(),
        };

        Self {
            playout_buffer: PlayoutBuffer::new(usize::from(playout_capacity), pkt.get_sequence().0),
            crypto_mode,
            decoder: OpusDecoder::new(sample_rate.into(), channels.into())
                .expect("Failed to create new Opus decoder for source."),
            decode_size: PacketDecodeSize::TwentyMillis,
            prune_time: Instant::now() + config.decode_state_timeout.into(),
            disconnected: false,
            channels,
        }
    }

    pub fn reconfigure_decoder(&mut self, config: DecodeConfig) {
        self.decoder = OpusDecoder::new(config.sample_rate.into(), config.channels.into())
            .expect("Failed to create new Opus decoder for source.");

        self.channels = config.channels;
    }

    pub fn store_packet(&mut self, packet: StoredPacket, config: &Config) {
        self.playout_buffer.store_packet(packet, config);
    }

    pub fn refresh_timer(&mut self, state_timeout: Duration) {
        if !self.disconnected {
            self.prune_time = Instant::now() + state_timeout;
        }
    }

    pub fn get_voice_tick(&mut self, config: &Config) -> Result<Option<VoiceData>> {
        // Acquire a packet from the playout buffer:
        // Update nexts, lasts...
        // different cases: null packet who we want to decode as a miss, and packet who we must ignore temporarily.
        let m_pkt = self.playout_buffer.fetch_packet(config);
        let pkt = match m_pkt {
            PacketLookup::Packet(StoredPacket { packet, decrypted }) => Some((packet, decrypted)),
            PacketLookup::MissedPacket => None,
            PacketLookup::Filling => return Ok(None),
        };

        let mut out = VoiceData {
            packet: None,
            decoded_voice: None,
        };

        let should_decode = config.decode_mode.should_decode();
        if let Some((packet, decrypted)) = pkt {
            let rtp = RtpPacket::new(&packet).unwrap();
            let extensions = rtp.get_extension() != 0;

            let payload = rtp.payload();
            let payload_offset = self.crypto_mode.payload_prefix_len();
            let mut payload_end_pad = payload
                .len()
                .checked_sub(self.crypto_mode.payload_suffix_len())
                .ok_or(Error::IllegalVoicePacket)?;
            if decrypted && rtp.get_padding() != 0 {
                let padding = usize::from(
                    *payload
                        .get(payload_end_pad.wrapping_sub(1))
                        .ok_or(Error::IllegalVoicePacket)?,
                );
                if padding == 0 {
                    return Err(Error::IllegalVoicePacket);
                }
                payload_end_pad = payload_end_pad
                    .checked_sub(padding)
                    .filter(|&end| end >= payload_offset)
                    .ok_or(Error::IllegalVoicePacket)?;
            }

            // We still need to compute missed packets here in case of long loss chains or similar.
            // This occurs due to the fallback in 'store_packet' (i.e., empty buffer and massive seq difference).
            // Normal losses should be handled by the below `else` branch.
            let new_seq: u16 = rtp.get_sequence().into();
            let missed_packets = new_seq.saturating_sub(self.playout_buffer.next_seq().0);

            // TODO: maybe hand over audio and extension indices alongside packet?
            let (audio, _packet_size) = self.scan_and_decode(
                payload
                    .get(payload_offset..payload_end_pad)
                    .ok_or(Error::IllegalVoicePacket)?,
                extensions,
                missed_packets,
                should_decode && decrypted,
            )?;

            let payload_end_pad = payload.len() - payload_end_pad;
            let rtp_data = RtpData {
                packet,
                payload_offset,
                payload_end_pad,
            };

            out.packet = Some(rtp_data);
            out.decoded_voice = audio;
        } else if should_decode {
            let mut audio = vec![0; self.decode_size.len()];
            let dest_samples = (&mut audio[..])
                .try_into()
                .expect("Decode logic will cap decode buffer size at i32::MAX.");
            let len = self.decoder.decode(&[], dest_samples, false)?;
            audio.truncate(2 * len);

            out.decoded_voice = Some(audio);
        }

        Ok(Some(out))
    }

    fn scan_and_decode(
        &mut self,
        data: &[u8],
        extension: bool,
        missed_packets: u16,
        decode: bool,
    ) -> Result<(Option<Vec<i16>>, usize)> {
        let start = if extension {
            RtpExtensionPacket::new(data)
                .map(|pkt| pkt.packet_size())
                .ok_or_else(|| Error::IllegalVoicePacket)
        } else {
            Ok(0)
        }?;

        let pkt = if decode {
            let mut out = vec![0; self.decode_size.len()];

            for _ in 0..missed_packets {
                let dest_samples = (&mut out[..])
                    .try_into()
                    .expect("Decode logic will cap decode buffer size at i32::MAX.");
                let _ = self.decoder.decode(&[], dest_samples, false);
            }

            loop {
                let slice_to_decode = &data[start..];

                let tried_audio_len = self.decoder.decode(slice_to_decode, &mut out, false);
                match tried_audio_len {
                    Ok(audio_len) => {
                        out.truncate(self.channels.channels() * audio_len);
                        break;
                    }
                    Err(e) if e.code() == ErrorCode::BufferTooSmall => {
                        if self.decode_size.can_bump_up() {
                            self.decode_size = self.decode_size.bump_up();
                            out = vec![0; self.decode_size.len()];
                        } else {
                            return Err(Error::IllegalVoicePacket);
                        }
                    }
                    Err(e) => {
                        return Err(e.into());
                    }
                }
            }

            Some(out)
        } else {
            None
        };

        Ok((pkt, data.len() - start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::num::NonZeroU8;

    fn packet_state(padding: Option<u8>, extension: bool) -> (SsrcState, Config) {
        let config = Config::default()
            .decode_mode(DecodeMode::Decode(DecodeConfig::default()))
            .playout_buffer_length(NonZeroU8::new(1).unwrap());
        // An Opus silence frame, four RTP padding bytes, then transport MAC/nonce.
        let flags =
            0x80 | if padding.is_some() { 0x20 } else { 0 } | if extension { 0x10 } else { 0 };
        let mut bytes = vec![flags, 0x78, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1];
        if extension {
            bytes.extend_from_slice(&[0xbe, 0xde, 0, 1, 0, 0, 0, 0]);
        }
        bytes.extend_from_slice(&[0xf8, 0xff, 0xfe]);
        if let Some(padding) = padding {
            bytes.extend_from_slice(&[0, 0, 0, padding]);
        }
        bytes.resize(bytes.len() + CryptoMode::Aes256Gcm.payload_suffix_len(), 0);
        let rtp = RtpPacket::new(&bytes).unwrap();
        let mut state = SsrcState::new(&rtp, CryptoMode::Aes256Gcm, &config);
        state.store_packet(
            StoredPacket {
                packet: Bytes::from(bytes),
                decrypted: true,
            },
            &config,
        );
        (state, config)
    }

    #[test]
    fn decodes_voice_without_rtp_padding() {
        let (mut state, config) = packet_state(Some(4), false);
        let data = state.get_voice_tick(&config).unwrap().unwrap();
        assert_eq!(data.decoded_voice.unwrap().len(), 1920);
        assert_eq!(
            data.packet.unwrap().payload_end_pad,
            4 + CryptoMode::Aes256Gcm.payload_suffix_len()
        );
    }

    #[test]
    fn decodes_unpadded_voice_and_rtp_extensions() {
        for extension in [false, true] {
            for padding in [None, Some(4)] {
                let (mut state, config) = packet_state(padding, extension);
                let data = state.get_voice_tick(&config).unwrap().unwrap();
                assert_eq!(data.decoded_voice.unwrap().len(), 1920);
                assert_eq!(
                    data.packet.unwrap().payload_end_pad,
                    CryptoMode::Aes256Gcm.payload_suffix_len()
                        + if padding.is_some() { 4 } else { 0 }
                );
            }
        }
    }

    #[test]
    fn rejects_invalid_rtp_padding() {
        for padding in [0, 255] {
            let (mut state, config) = packet_state(Some(padding), false);
            assert!(matches!(
                state.get_voice_tick(&config),
                Err(Error::IllegalVoicePacket)
            ));
        }
    }
}
