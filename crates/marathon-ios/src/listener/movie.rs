//! U267: a received movie reaches the report only if its box structure is complete; Rust-only support for the port of
//! vendor/vendor-apple/ios/src/main/kotlin/com/malinskiy/marathon/apple/ios/listener/video/ScreenRecordingListener.kt.
//!
//! simctl can exit cleanly after INT on a recording a few hundred milliseconds long and still leave an empty or
//! unfinished movie. A transcode of it fails, and the fallback pulled that original and published it as is. The check
//! runs in the receive's own D97 stage (`marathon_apple::cmd::ReceiveCheck`), so a rejected movie never reaches the
//! report path and an earlier file there stays.
//!
//! It is a structural check, not a playability guarantee: it reads box headers and none of the media.
use std::{
    fs,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

/// Why a received movie was not published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Incomplete {
    Empty,
    /// A box's size runs past the end of the file, or the file ends inside a box header: the writer stopped early.
    Truncated,
    /// No top-level `moov`: the sample tables a player needs were never written, as when the writer did not finalize.
    NoMovieBox,
    /// A `moov` without `mvhd` or without any `trak`: a header with nothing to play.
    EmptyMovieBox,
    /// No `mdat` with any bytes in it: there is no media to play.
    NoMediaData,
    /// Not a box structure at all: a size smaller than its own header.
    NotAMovie,
}
impl Incomplete {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Empty => "the recording is empty",
            Self::Truncated => "the recording is not a complete movie (it is cut short)",
            Self::NoMovieBox => "the recording is not a complete movie (it has no moov box)",
            Self::EmptyMovieBox => "the recording is not a complete movie (its moov box has no track)",
            Self::NoMediaData => "the recording is not a complete movie (it has no media data)",
            Self::NotAMovie => "the recording is not a movie",
        }
    }
}

/// One box header: its type, where its body starts and where the box ends.
struct BoxHeader {
    kind: [u8; 4],
    body: u64,
    end: u64,
}

/// Reads box headers in file order. A seek within the buffered window keeps the buffer, so a run of small boxes costs
/// one read per 8 KiB and a large box one seek: the walk reads at most the file once, whatever its box count.
struct Boxes {
    file: BufReader<fs::File>,
    /// Where the next byte read from `file` comes from.
    at: u64,
}
impl Boxes {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        match i64::try_from(offset.wrapping_sub(self.at)) {
            Ok(delta) if offset >= self.at => self.file.seek_relative(delta)?,
            _ => {
                self.file.seek(SeekFrom::Start(offset))?;
            }
        }
        self.file.read_exact(buf)?;
        self.at = offset + buf.len() as u64;
        Ok(())
    }

    /// Reads the header of the box at `offset`, which must lie within `[offset, end)`.
    fn header(&mut self, offset: u64, end: u64) -> io::Result<Result<BoxHeader, Incomplete>> {
        if end - offset < 8 {
            return Ok(Err(Incomplete::Truncated));
        }
        let mut head = [0u8; 8];
        self.read_at(offset, &mut head)?;
        let kind = [head[4], head[5], head[6], head[7]];
        let short = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
        let (size, mut body) = match short {
            // A size of 0 runs to the end of the enclosing extent (for a top-level box, the end of the file).
            0 => (end - offset, offset + 8),
            1 => {
                if end - offset < 16 {
                    return Ok(Err(Incomplete::Truncated));
                }
                let mut large = [0u8; 8];
                self.read_at(offset + 8, &mut large)?;
                (u64::from_be_bytes(large), offset + 16)
            }
            size => (u64::from(size), offset + 8),
        };
        // A `uuid` box's header goes on with its 16-byte user type.
        if &kind == b"uuid" {
            body += 16;
        }
        if size < body - offset {
            // A size of 0 is the rest of the extent, which here ends inside the header.
            return Ok(Err(if short == 0 {
                Incomplete::Truncated
            } else {
                Incomplete::NotAMovie
            }));
        }
        if size > end - offset {
            return Ok(Err(Incomplete::Truncated));
        }
        Ok(Ok(BoxHeader {
            kind,
            body,
            end: offset + size,
        }))
    }
}

/// Checks an MP4 or QuickTime file by its box structure: every top-level box lies within the file, a `moov` holds an
/// `mvhd` and at least one `trak`, and an `mdat` holds at least one byte. It reads box headers only, so it costs no
/// worker command, needs no ffmpeg and reads none of the media; its work is bounded by the file's size (a box takes at
/// least 8 bytes), not by a box count, so that no complete movie is rejected for having many fragments. Blocking.
///
/// An `mdat` with no bytes is rejected: whatever the sample tables say, there is nothing to decode, which is what a
/// writer stopped right after its first header leaves. A fragmented MP4 keeps its samples in later `mdat`s, so one
/// non-empty `mdat` anywhere is enough.
pub fn check(path: &Path) -> io::Result<Result<(), Incomplete>> {
    let file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(Err(Incomplete::Empty));
    }
    let mut file = Boxes {
        file: BufReader::new(file),
        at: 0,
    };
    let mut offset = 0u64;
    let (mut movie, mut media) = (None, false);
    while offset < len {
        let b = match file.header(offset, len)? {
            Ok(b) => b,
            Err(why) => return Ok(Err(why)),
        };
        match &b.kind {
            b"moov" if movie.is_none() => movie = Some((b.body, b.end)),
            b"mdat" => media |= b.end > b.body,
            _ => {}
        }
        offset = b.end;
    }
    let Some((body, end)) = movie else {
        return Ok(Err(Incomplete::NoMovieBox));
    };
    if !media {
        return Ok(Err(Incomplete::NoMediaData));
    }
    let mut offset = body;
    let (mut header_box, mut track) = (false, false);
    while offset < end {
        let b = match file.header(offset, end)? {
            Ok(b) => b,
            Err(why) => return Ok(Err(why)),
        };
        header_box |= &b.kind == b"mvhd";
        track |= &b.kind == b"trak";
        offset = b.end;
    }
    Ok(if header_box && track {
        Ok(())
    } else {
        Err(Incomplete::EmptyMovieBox)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDING: &[u8] = include_bytes!("../../../../fixtures/fake-worker/apple/media/recording.mp4");
    const TRANSCODED: &[u8] = include_bytes!("../../../../fixtures/fake-worker/apple/media/transcoded.mp4");

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(body.len() + 8).unwrap().to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }
    fn checked(bytes: &[u8]) -> Result<(), Incomplete> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("movie.mp4");
        fs::write(&path, bytes).unwrap();
        check(&path).unwrap()
    }
    fn ftyp() -> Vec<u8> {
        boxed(b"ftyp", b"isom\0\0\x02\0isom")
    }
    /// A minimal complete `moov`: a movie header and one track.
    fn moov() -> Vec<u8> {
        let mut body = boxed(b"mvhd", &[0; 100]);
        body.extend(boxed(b"trak", &boxed(b"tkhd", &[0; 84])));
        boxed(b"moov", &body)
    }

    #[test]
    fn complete_movies_pass_whatever_their_layout() {
        // simctl's layout (moov after mdat) and ffmpeg's faststart layout (moov first).
        assert_eq!(checked(RECORDING), Ok(()));
        assert_eq!(checked(TRANSCODED), Ok(()));
        // A 64-bit mdat, then moov.
        let mut large = ftyp();
        large.extend_from_slice(&1u32.to_be_bytes());
        large.extend_from_slice(b"mdat");
        large.extend_from_slice(&(16u64 + 4).to_be_bytes());
        large.extend_from_slice(b"data");
        large.extend(moov());
        assert_eq!(checked(&large), Ok(()));
        // A last box whose size is 0 runs to the end of the file.
        let mut open = ftyp();
        open.extend(moov());
        open.extend_from_slice(&0u32.to_be_bytes());
        open.extend_from_slice(b"mdat");
        open.extend_from_slice(b"data");
        assert_eq!(checked(&open), Ok(()));
    }

    #[test]
    fn empty_truncated_and_foreign_files_are_rejected() {
        assert_eq!(checked(b""), Err(Incomplete::Empty));
        // An unfinalized recording: everything up to the media data, no moov.
        let moov_at = RECORDING.windows(4).position(|w| w == b"moov").unwrap() - 4;
        assert_eq!(checked(&RECORDING[..moov_at]), Err(Incomplete::NoMovieBox));
        // A moov box cut short.
        assert_eq!(checked(&RECORDING[..RECORDING.len() - 1]), Err(Incomplete::Truncated));
        // A faststart movie cut just after the mdat header: its moov is intact, its media is not.
        let mdat_at = TRANSCODED.windows(4).position(|w| w == b"mdat").unwrap() - 4;
        assert_eq!(checked(&TRANSCODED[..mdat_at + 8]), Err(Incomplete::Truncated));
        assert_eq!(checked(&TRANSCODED[..mdat_at]), Err(Incomplete::NoMediaData));
        // A file ending inside a box header.
        assert_eq!(checked(&TRANSCODED[..mdat_at + 3]), Err(Incomplete::Truncated));
        assert_eq!(checked(b"media"), Err(Incomplete::Truncated));
        assert_eq!(checked(b"not a movie at all"), Err(Incomplete::Truncated));
        // A box claiming to be shorter than its own header.
        let mut short = 4u32.to_be_bytes().to_vec();
        short.extend_from_slice(b"moovmoov");
        assert_eq!(checked(&short), Err(Incomplete::NotAMovie));
        // A 64-bit size smaller than its 16-byte header.
        let mut large = 1u32.to_be_bytes().to_vec();
        large.extend_from_slice(b"mdat");
        large.extend_from_slice(&8u64.to_be_bytes());
        assert_eq!(checked(&large), Err(Incomplete::NotAMovie));
    }

    #[test]
    fn a_movie_needs_a_track_and_media_data() {
        let mdat = boxed(b"mdat", b"data");
        let with = |moov: Vec<u8>, mdat: &[u8]| {
            let mut out = ftyp();
            out.extend(moov);
            out.extend_from_slice(mdat);
            checked(&out)
        };
        assert_eq!(with(moov(), &mdat), Ok(()));
        assert_eq!(with(boxed(b"moov", b""), &mdat), Err(Incomplete::EmptyMovieBox));
        assert_eq!(
            with(boxed(b"moov", &boxed(b"mvhd", &[0; 100])), &mdat),
            Err(Incomplete::EmptyMovieBox)
        );
        assert_eq!(
            with(boxed(b"moov", &boxed(b"trak", b"")), &mdat),
            Err(Incomplete::EmptyMovieBox)
        );
        // A child running past its moov is cut short, even though the file goes on.
        let mut overrun = boxed(b"mvhd", &[0; 100]);
        overrun.extend_from_slice(&64u32.to_be_bytes());
        overrun.extend_from_slice(b"trak");
        assert_eq!(with(boxed(b"moov", &overrun), &mdat), Err(Incomplete::Truncated));
        assert_eq!(with(moov(), b""), Err(Incomplete::NoMediaData));
        assert_eq!(with(moov(), &boxed(b"mdat", b"")), Err(Incomplete::NoMediaData));
        // A fragmented movie: an empty first mdat is fine when a later one holds the samples.
        let mut fragmented = boxed(b"mdat", b"");
        fragmented.extend(boxed(b"moof", b""));
        fragmented.extend(boxed(b"mdat", b"data"));
        assert_eq!(with(moov(), &fragmented), Ok(()));
    }

    #[test]
    fn many_fragments_or_tracks_are_no_reason_to_reject_a_movie() {
        // 200 000 one-sample fragments, as a fragmented recording of nearly two hours at 30 fps has: 400 000 top-level
        // boxes after the moov, four times round 2's cap, which rejected a complete 50 000-fragment movie.
        let mut fragments = ftyp();
        fragments.extend(moov());
        let fragment = [boxed(b"moof", b""), boxed(b"mdat", b"f")].concat();
        for _ in 0..200_000 {
            fragments.extend_from_slice(&fragment);
        }
        assert_eq!(checked(&fragments), Ok(()));
        // 10 000 tracks inside the moov.
        let mut tracks = boxed(b"mvhd", &[0; 100]);
        for _ in 0..10_000 {
            tracks.extend(boxed(b"trak", b""));
        }
        let mut wide = ftyp();
        wide.extend(boxed(b"moov", &tracks));
        wide.extend(boxed(b"mdat", b"data"));
        assert_eq!(checked(&wide), Ok(()));
    }

    #[test]
    fn a_uuid_box_needs_its_user_type() {
        let with = |uuid: &[u8]| {
            let mut out = ftyp();
            out.extend_from_slice(uuid);
            out.extend(moov());
            out.extend(boxed(b"mdat", b"data"));
            checked(&out)
        };
        assert_eq!(with(&boxed(b"uuid", &[7; 16])), Ok(()));
        assert_eq!(with(&boxed(b"uuid", &[7; 20])), Ok(()));
        assert_eq!(with(&boxed(b"uuid", b"")), Err(Incomplete::NotAMovie));
        assert_eq!(with(&boxed(b"uuid", &[7; 15])), Err(Incomplete::NotAMovie));
        // With a 64-bit size the user type follows the 16-byte header.
        let mut large = 1u32.to_be_bytes().to_vec();
        large.extend_from_slice(b"uuid");
        large.extend_from_slice(&(16u64 + 16).to_be_bytes());
        large.extend_from_slice(&[7; 16]);
        assert_eq!(with(&large), Ok(()));
        let mut large = 1u32.to_be_bytes().to_vec();
        large.extend_from_slice(b"uuid");
        large.extend_from_slice(&(16u64 + 8).to_be_bytes());
        large.extend_from_slice(&[7; 8]);
        assert_eq!(with(&large), Err(Incomplete::NotAMovie));
        // A last uuid box of size 0 that ends inside its user type is cut short.
        let mut open = ftyp();
        open.extend(moov());
        open.extend(boxed(b"mdat", b"data"));
        open.extend_from_slice(&0u32.to_be_bytes());
        open.extend_from_slice(b"uuid");
        open.extend_from_slice(&[7; 15]);
        assert_eq!(checked(&open), Err(Incomplete::Truncated));
        open.push(7);
        assert_eq!(checked(&open), Ok(()));
    }
}
