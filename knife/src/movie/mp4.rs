//! MP4 demuxing (after shin-video's `mp4.rs`): the movie's H.264 video track and AAC audio track.

use std::{
    fs::File,
    io::{BufReader, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use anyhow::{Context, Result, anyhow};
use mp4::{Mp4Sample, Mp4Track, TrackType};
use parking_lot::Mutex;

type Reader = Arc<Mutex<mp4::Mp4Reader<BufReader<File>>>>;

/// Reads one track's samples in decode order.
pub struct TrackReader {
    mp4: Reader,
    track_id: u32,
    /// 1-based number of the next sample
    position: u32,
    count: u32,
}

impl TrackReader {
    fn new(mp4: Reader, track_id: u32) -> Result<Self> {
        let count = mp4
            .lock()
            .tracks()
            .get(&track_id)
            .context("missing mp4 track")?
            .sample_count();
        Ok(Self {
            mp4,
            track_id,
            position: 1,
            count,
        })
    }

    pub fn info<R>(&self, f: impl FnOnce(&Mp4Track) -> R) -> R {
        let mp4 = self.mp4.lock();
        f(mp4.tracks().get(&self.track_id).unwrap())
    }

    /// Length of the track's edit list in seconds (movie timescale), when it has one.
    pub fn edit_duration(&self) -> Option<f64> {
        let mp4 = self.mp4.lock();
        let timescale = f64::from(mp4.timescale().max(1));
        let elst = mp4.tracks().get(&self.track_id)?.trak.edts.as_ref()?.elst.as_ref()?;
        let total: u64 = elst.entries.iter().map(|e| e.segment_duration).sum();
        (total > 0).then(|| total as f64 / timescale)
    }

    /// Continue from sample `index` (0-based, decode order).
    pub fn seek(&mut self, index: u32) {
        self.position = index + 1;
    }

    pub fn next_sample(&mut self) -> Result<Option<Mp4Sample>> {
        if self.position > self.count {
            return Ok(None);
        }
        let sample = self
            .mp4
            .lock()
            .read_sample(self.track_id, self.position)
            .with_context(|| {
                format!(
                    "reading sample {} of track {}",
                    self.position, self.track_id
                )
            })?
            .ok_or_else(|| anyhow!("mp4 sample {} missing", self.position))?;
        self.position += 1;
        Ok(Some(sample))
    }
}

pub struct Mp4 {
    pub video: TrackReader,
    pub audio: Option<TrackReader>,
}

impl Mp4 {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let size = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        let mp4 = mp4::Mp4Reader::read_header(BufReader::new(file), size)
            .context("reading the MP4 headers")?;
        let find = |ty: TrackType| {
            mp4.tracks()
                .values()
                .find(|t| t.track_type().is_ok_and(|t| t == ty))
                .map(Mp4Track::track_id)
        };
        let video = find(TrackType::Video).context("no video track")?;
        let audio = find(TrackType::Audio);
        let mp4 = Arc::new(Mutex::new(mp4));
        Ok(Self {
            video: TrackReader::new(mp4.clone(), video)?,
            audio: audio.map(|id| TrackReader::new(mp4, id)).transpose()?,
        })
    }
}

/// Presentation timestamps of all samples in decode order, from the sample tables.
pub fn presentation_times(track: &Mp4Track) -> Vec<i64> {
    let stbl = &track.trak.mdia.minf.stbl;
    let deltas = stbl
        .stts
        .entries
        .iter()
        .flat_map(|e| std::iter::repeat_n(i64::from(e.sample_delta), e.sample_count as usize));
    let offsets = stbl
        .ctts
        .iter()
        .flat_map(|c| &c.entries)
        .flat_map(|e| std::iter::repeat_n(i64::from(e.sample_offset), e.sample_count as usize))
        .chain(std::iter::repeat(0));
    deltas
        .scan(0i64, |t, d| {
            let start = *t;
            *t += d;
            Some(start)
        })
        .zip(offsets)
        .map(|(decode, offset)| decode + offset)
        .collect()
}

/// Sync (IDR) samples, 0-based in decode order; every sample when the track has no sync table.
pub fn sync_samples(track: &Mp4Track) -> Vec<u32> {
    match &track.trak.mdia.minf.stbl.stss {
        Some(stss) => stss.entries.iter().map(|&n| n.saturating_sub(1)).collect(),
        None => (0..track.sample_count()).collect(),
    }
}
