//! The movie's AAC track, decoded with symphonia (after shin-video's `AacFrameSource`).

use anyhow::{Context, Result, bail};
use symphonia::core::{
    audio::{Audio, GenericAudioBufferRef},
    codecs::audio::{
        AudioCodecParameters, AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_AAC,
    },
    packet::PacketRef,
};

use super::mp4::TrackReader;

/// Decode the whole track to stereo frames; returns the sample rate too. Starts at sample 0 with the encoder
/// priming kept, as the movies' edit lists start at media time 0, and ends with the edit list.
pub fn decode(mut track: TrackReader) -> Result<(u32, Vec<[f32; 2]>)> {
    let mp4a = track
        .info(|t| t.trak.mdia.minf.stbl.stsd.mp4a.clone())
        .context("no mp4a atom")?;
    let mut params = AudioCodecParameters::new();
    params
        .for_codec(CODEC_ID_AAC)
        .with_sample_rate(mp4a.samplerate.value() as u32);
    if let Some(esds) = mp4a.esds {
        let d = esds.es_desc.dec_config.dec_specific;
        // the AudioSpecificConfig, serialized again
        params.extra_data = Some(
            vec![
                (d.profile << 3) + (d.freq_index >> 1),
                (d.freq_index << 7) + (d.chan_conf << 3),
            ]
            .into_boxed_slice(),
        );
    }
    let mut decoder =
        symphonia::default::codecs::AacDecoder::try_new(&params, &AudioDecoderOptions::default())
            .context("creating the AAC decoder")?;
    let rate = decoder
        .codec_params()
        .sample_rate
        .context("AAC sample rate missing")?;
    let mut frames = Vec::new();
    while let Some(sample) = track.next_sample()? {
        let packet = PacketRef::new(
            1,
            (sample.start_time as i64).into(),
            sample.duration.into(),
            &sample.bytes,
        );
        let GenericAudioBufferRef::F32(buf) =
            decoder.decode_ref(&packet).context("decoding AAC")?
        else {
            bail!("unexpected AAC sample format");
        };
        match buf.num_planes() {
            1 => frames.extend(buf.plane(0).unwrap().iter().map(|&s| [s, s])),
            2 => frames.extend(
                buf.plane(0)
                    .unwrap()
                    .iter()
                    .zip(buf.plane(1).unwrap())
                    .map(|(&l, &r)| [l, r]),
            ),
            n => bail!("unsupported AAC channel count {n}"),
        }
    }
    // the last packet's padding runs past the edit list, which ends the track
    if let Some(duration) = track.edit_duration() {
        frames.truncate((duration * f64::from(rate)).round() as usize);
    }
    Ok((rate, frames))
}
