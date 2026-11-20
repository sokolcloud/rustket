//! Rocket.Chat's own notification sounds (`assets/sounds`, MIT, see the
//! LICENSE there), bundled as MP3. A sound is decoded on first use,
//! scaled to its volume and written as 16-bit PCM WAV to the cache
//! directory, which every platform player can play.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::default::codecs::MpaDecoder;
use symphonia::default::formats::MpaReader;

/// The bundled sounds: the message sounds Account → Preferences offers
/// (`newRoomNotification`'s default `door` among them).
pub const SOUNDS: &[(&str, &[u8])] = &[
    ("beep", include_bytes!("../../../assets/sounds/beep.mp3")),
    (
        "chelle",
        include_bytes!("../../../assets/sounds/chelle.mp3"),
    ),
    ("chime", include_bytes!("../../../assets/sounds/chime.mp3")),
    ("ding", include_bytes!("../../../assets/sounds/ding.mp3")),
    ("door", include_bytes!("../../../assets/sounds/door.mp3")),
    (
        "droplet",
        include_bytes!("../../../assets/sounds/droplet.mp3"),
    ),
    (
        "highbell",
        include_bytes!("../../../assets/sounds/highbell.mp3"),
    ),
    (
        "seasons",
        include_bytes!("../../../assets/sounds/seasons.mp3"),
    ),
];

/// The MP3 of bundled sound `name`.
pub fn bundled(name: &str) -> Option<&'static [u8]> {
    SOUNDS.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unknown sound {0}")]
    Unknown(String),
    #[error("decode: {0}")]
    Decode(#[from] symphonia::core::errors::Error),
    #[error("no audio")]
    Empty,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Decoded audio: interleaved samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Pcm {
    pub rate: u32,
    pub channels: u16,
    pub samples: Vec<i16>,
}

/// The audio after an ID3v2 tag at the start (the MP3 reader expects a
/// frame there).
fn skip_id3v2(mp3: &[u8]) -> &[u8] {
    if mp3.len() < 10 || &mp3[..3] != b"ID3" {
        return mp3;
    }
    // Syncsafe size of the tag body; a footer adds 10 bytes.
    let size = mp3[6..10]
        .iter()
        .fold(0usize, |acc, b| (acc << 7) | usize::from(b & 0x7f));
    let footer = if mp3[5] & 0x10 != 0 { 10 } else { 0 };
    mp3.get(10 + size + footer..).unwrap_or(&[])
}

/// Decodes an MP3.
pub fn decode(mp3: &[u8]) -> Result<Pcm, Error> {
    let audio = skip_id3v2(mp3).to_vec();
    let mss = MediaSourceStream::new(
        Box::new(Cursor::new(audio)),
        MediaSourceStreamOptions::default(),
    );
    let mut reader = MpaReader::try_new(mss, FormatOptions::default())?;
    let params = reader
        .tracks()
        .first()
        .and_then(|t| t.codec_params.as_ref())
        .and_then(|p| p.audio())
        .cloned()
        .ok_or(Error::Empty)?;
    let mut decoder = MpaDecoder::try_new(&params, &AudioDecoderOptions::default())?;
    let mut samples: Vec<i16> = Vec::new();
    let mut spec: Option<(u32, u16)> = None;
    let mut frame: Vec<i16> = Vec::new();
    while let Some(packet) = reader.next_packet()? {
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            // A damaged frame: skip it.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(e) => return Err(e.into()),
        };
        let channels = u16::try_from(buf.spec().channels().count()).unwrap_or(0);
        if channels == 0 {
            continue;
        }
        spec.get_or_insert((buf.spec().rate(), channels));
        buf.copy_to_vec_interleaved(&mut frame);
        samples.extend_from_slice(&frame);
    }
    let (rate, channels) = spec.ok_or(Error::Empty)?;
    if samples.is_empty() {
        return Err(Error::Empty);
    }
    Ok(Pcm {
        rate,
        channels,
        samples,
    })
}

/// Scales the samples to `volume` percent (0..=100).
pub fn scale(samples: &mut [i16], volume: u8) {
    let f = f32::from(volume.min(100)) / 100.0;
    for s in samples {
        *s = (f32::from(*s) * f).round() as i16;
    }
}

/// A 16-bit PCM WAV file of `pcm`.
pub fn wav(pcm: &Pcm) -> Vec<u8> {
    let data_len = (pcm.samples.len() * 2) as u32;
    let block_align = pcm.channels * 2;
    let byte_rate = pcm.rate * u32::from(block_align);
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&pcm.channels.to_le_bytes());
    out.extend_from_slice(&pcm.rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in &pcm.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Version of the cached WAV files: bumped when the decoding, the scaling
/// or a bundled sound changes, so old files are not reused.
const CACHE_VERSION: u32 = 1;

/// Cache file of sound `name` at `volume` percent.
pub fn cache_file(dir: &Path, name: &str, volume: u8) -> PathBuf {
    dir.join(format!("v{CACHE_VERSION}-{name}-{volume}.wav"))
}

/// A cached file is usable: a WAV header at least (a crash can leave an
/// empty or cut file behind).
fn valid_wav(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 44];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut head))
        .is_ok_and(|()| &head[..4] == b"RIFF" && &head[8..12] == b"WAVE")
}

/// The WAV of bundled sound `name` at `volume` percent in `dir`, decoded
/// and written on first use (again when the cached file is damaged).
/// Blocking.
pub fn wav_file(dir: &Path, name: &str, volume: u8) -> Result<PathBuf, Error> {
    use std::io::Write;
    let mp3 = bundled(name).ok_or_else(|| Error::Unknown(name.to_owned()))?;
    let path = cache_file(dir, name, volume);
    if valid_wav(&path) {
        return Ok(path);
    }
    let mut pcm = decode(mp3)?;
    scale(&mut pcm.samples, volume);
    std::fs::create_dir_all(dir)?;
    // Written aside, synced, then renamed: a player never sees half a file.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}-{volume}.{}-{seq}.tmp", std::process::id()));
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(&wav(&pcm))?;
        f.sync_all()
    });
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        // Another player won the race (Windows refuses to replace a file
        // in use): its file is as good.
        if valid_wav(&path) {
            return Ok(path);
        }
        return Err(e.into());
    }
    Ok(path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_sound_decodes() {
        for (name, mp3) in SOUNDS {
            let pcm = decode(mp3).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(pcm.rate >= 8000, "{name}");
            assert!((1..=2).contains(&pcm.channels), "{name}");
            let secs = pcm.samples.len() as f32 / pcm.rate as f32 / f32::from(pcm.channels);
            assert!((0.05..10.0).contains(&secs), "{name}: {secs}s");
            assert!(
                pcm.samples.iter().any(|s| s.unsigned_abs() > 1000),
                "{name}"
            );
        }
        assert!(bundled("telephone").is_none());
        assert!(decode(b"not an mp3").is_err());
    }

    #[test]
    fn id3_tags_are_skipped() {
        let mut tagged = b"ID3\x04\x00\x00\x00\x00\x01\x00".to_vec();
        tagged.extend(std::iter::repeat_n(0u8, 128));
        tagged.extend_from_slice(b"\xff\xfb");
        assert_eq!(skip_id3v2(&tagged), b"\xff\xfb");
        assert_eq!(skip_id3v2(b"\xff\xfb\x90"), b"\xff\xfb\x90");
        assert_eq!(skip_id3v2(b"ID3\x04\x00\x00\x7f\x7f\x7f\x7f"), b"");
    }

    #[test]
    fn damaged_cache_files_are_written_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = cache_file(dir.path(), "ding", 40);
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("v1-")
        );
        std::fs::write(&path, b"").unwrap();
        assert_eq!(wav_file(dir.path(), "ding", 40).unwrap(), path);
        assert!(valid_wav(&path));
        let len = std::fs::metadata(&path).unwrap().len();
        assert!(len > 44);
        // A valid file is reused as is; nothing is left aside.
        assert_eq!(wav_file(dir.path(), "ding", 40).unwrap(), path);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert!(matches!(
            wav_file(dir.path(), "telephone", 40),
            Err(Error::Unknown(_))
        ));
    }

    #[test]
    fn volume_scales_samples() {
        let mut s = [1000, -1000, i16::MAX, i16::MIN];
        scale(&mut s, 50);
        assert_eq!(s, [500, -500, 16384, -16384]);
        let mut s = [1000];
        scale(&mut s, 100);
        assert_eq!(s, [1000]);
        scale(&mut s, 0);
        assert_eq!(s, [0]);
    }

    #[test]
    fn wav_header() {
        let w = wav(&Pcm {
            rate: 44100,
            channels: 2,
            samples: vec![1, -1, 2, -2],
        });
        assert_eq!(w.len(), 44 + 8);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 8);
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(u16::from_le_bytes([w[22], w[23]]), 2);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 44100);
        assert_eq!(u32::from_le_bytes(w[28..32].try_into().unwrap()), 44100 * 4);
        assert_eq!(u16::from_le_bytes([w[34], w[35]]), 16);
        assert_eq!(&w[36..40], b"data");
        assert_eq!(&w[44..46], &1i16.to_le_bytes());
    }

    #[test]
    fn wav_files_are_cached_per_name_and_volume() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("sounds");
        let full = wav_file(&dir, "chime", 100).unwrap();
        assert_eq!(full, dir.join("v1-chime-100.wav"));
        let half = wav_file(&dir, "chime", 50).unwrap();
        assert_ne!(full, half);
        let (a, b) = (std::fs::read(&full).unwrap(), std::fs::read(&half).unwrap());
        assert_eq!(a.len(), b.len());
        let peak = |w: &[u8]| {
            w[44..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u32::from(i16::from_le_bytes(*c).unsigned_abs()))
                .max()
                .unwrap()
        };
        assert!(peak(&b) < peak(&a) * 6 / 10, "{} {}", peak(&b), peak(&a));
        // Cached: a valid file is not written again.
        let mut marked = a.clone();
        let last = marked.len() - 1;
        marked[last] ^= 1;
        std::fs::write(&full, &marked).unwrap();
        assert_eq!(
            std::fs::read(wav_file(&dir, "chime", 100).unwrap()).unwrap(),
            marked
        );
        assert!(matches!(
            wav_file(&dir, "../x", 100),
            Err(Error::Unknown(_))
        ));
        assert!(
            std::fs::read_dir(&dir)
                .unwrap()
                .all(|e| { !e.unwrap().file_name().to_string_lossy().ends_with(".tmp") })
        );
    }
}
