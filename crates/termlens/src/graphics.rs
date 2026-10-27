//! Inline graphics: the payloads an application transmitted, and — with the
//! `decode` feature — what they depicted.
//!
//! Two protocols reach a terminal as escape strings rather than as cells:
//! kitty's (`APC G <control> ; <base64> ST`) and sixel's
//! (`DCS <params> q <data> ST`). Neither touches the grid, so no content
//! predicate can see one, and until a payload is *captured* the only
//! questions a test can ask are "did anything go out?" and "how big was it?".
//!
//! Capturing is not rendering. termlens draws no pixels and goes on declining
//! both protocols in DA1 unless a test declares otherwise with
//! [`TerminalBuilder::graphics`](crate::TerminalBuilder::graphics) — which is
//! precisely why an application that transmits one anyway is worth catching.
//! What capture adds is the other half of the assertion: *which* image, of
//! what size, placed where, and — decoded — of what colour.
//!
//! This is the same position [`Clipboard`](crate::Clipboard) takes on
//! `OSC 52`. A write is not a question, the application's own toast proves
//! only that the code path ran, and the payload is usually the behaviour
//! actually under test.
//!
//! ```no_run
//! # fn main() -> termlens::Result<()> {
//! # let mut t = termlens::Terminal::builder().spawn("true")?;
//! let screen = t.wait_frame(|s| s.contains("ready"))?;
//! let seen = screen.graphics();
//! let image = seen.payloads().last().expect("the chart went out as an image");
//! // Placed on exactly the character cells the layout reserved.
//! assert_eq!(image.cells(), Some((106, 7)));
//! # Ok(())
//! # }
//! ```

use std::fmt;
use std::sync::Arc;

/// How many payload bytes are kept for inspection by default: enough for a
/// screenful of chart at any plausible cell size, and small enough that a
/// suite which never looks at an image pays nothing it would notice.
///
/// [`TerminalBuilder::capture_graphics`](crate::TerminalBuilder::capture_graphics)
/// moves it, in either direction.
pub(crate) const DEFAULT_CAPTURE: usize = 4 << 20;

/// The most payloads retained at once, however small they are. A sliding
/// window like scrollback's: an application that redraws for an hour must
/// not grow this without limit, and it is the recent ones an assertion is
/// about.
pub(crate) const HISTORY: usize = 512;

/// Which protocol carried a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum GraphicsProtocol {
    /// The kitty graphics protocol: `APC G <control> ; <base64> ST`.
    Kitty,
    /// Sixel: `DCS <params> q <data> ST`.
    Sixel,
}

impl fmt::Display for GraphicsProtocol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GraphicsProtocol::Kitty => "kitty",
            GraphicsProtocol::Sixel => "sixel",
        })
    }
}

/// What the application asked the terminal to *do* with an image.
///
/// Only kitty distinguishes these; a sixel is always drawn where the cursor
/// stands, so it reports [`TransmitAndPlace`](Self::TransmitAndPlace).
///
/// The distinction is not cosmetic: a delete carries no picture, and counting
/// one as an image transmitted makes "how many images did this frame send?"
/// answer with the number of *escapes* instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum GraphicsAction {
    /// `a=t`: transmit the data, place it later.
    Transmit,
    /// `a=p`: place an image transmitted earlier.
    Place,
    /// `a=T`, and every sixel: transmit and place in one go.
    TransmitAndPlace,
    /// `a=d`: delete an image, or the placements made from one.
    Delete,
    /// `a=f` (transmit a frame), `a=a` (animate), `a=c` (compose) and
    /// anything else the protocol grows. Named rather than folded into
    /// [`Transmit`](Self::Transmit): guessing that an unknown action carries
    /// a picture is how a count starts lying.
    Other,
}

impl GraphicsAction {
    /// Whether this action carries image data — the question
    /// [`GraphicsSeen::total`] is counting.
    #[must_use]
    pub fn carries_image(self) -> bool {
        matches!(
            self,
            GraphicsAction::Transmit | GraphicsAction::TransmitAndPlace
        )
    }
}

/// How the pixels in a payload are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[cfg_attr(
    feature = "serde",
    derive(serde::Serialize, serde::Deserialize),
    serde(rename_all = "snake_case")
)]
pub enum GraphicsFormat {
    /// kitty `f=24`: three bytes a pixel.
    Rgb,
    /// kitty `f=32`: four bytes a pixel.
    Rgba,
    /// kitty `f=100`: a PNG file. Decoding one is out of scope — termlens
    /// carries no image codec — so `GraphicsPayload::decode` reports it
    /// as unsupported rather than guessing.
    Png,
    /// The sixel data stream itself.
    Sixel,
    /// A kitty `f=` value this crate does not know.
    Other(u32),
}

impl fmt::Display for GraphicsFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphicsFormat::Rgb => f.write_str("rgb"),
            GraphicsFormat::Rgba => f.write_str("rgba"),
            GraphicsFormat::Png => f.write_str("png"),
            GraphicsFormat::Sixel => f.write_str("sixel"),
            GraphicsFormat::Other(value) => write!(f, "f={value}"),
        }
    }
}

/// Where a kitty transmission says its bytes are (`t=`).
///
/// Only [`Direct`](Self::Direct) carries the image in the escape itself.
/// The rest name something outside the stream — a path, or a shared-memory
/// object — so the payload body is a *name*, not pixels, and
/// `GraphicsPayload::decode` refuses it rather than decoding the name
/// (#402). termlens never opens the path or the mapping: what an
/// application under test points at is not something a harness should read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GraphicsTransmission {
    /// kitty `t=d`, and the default when a transmission names no medium:
    /// the bytes are in the escape.
    Direct,
    /// kitty `t=f`: the body is the path of a file to read.
    File,
    /// kitty `t=t`: the body is the path of a temporary file, to be
    /// deleted after reading.
    TempFile,
    /// kitty `t=s`: the body is the name of a POSIX shared-memory object.
    SharedMemory,
    /// A kitty `t=` value this crate does not know. Decoding refuses it:
    /// an unknown medium is exactly as likely to be indirect as not, and
    /// guessing is how the body of one got decoded as pixels to begin with.
    Other(u8),
}

impl GraphicsTransmission {
    /// The byte the application wrote.
    #[must_use]
    fn code(self) -> u8 {
        match self {
            GraphicsTransmission::Direct => b'd',
            GraphicsTransmission::File => b'f',
            GraphicsTransmission::TempFile => b't',
            GraphicsTransmission::SharedMemory => b's',
            GraphicsTransmission::Other(value) => value,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            b'd' => GraphicsTransmission::Direct,
            b'f' => GraphicsTransmission::File,
            b't' => GraphicsTransmission::TempFile,
            b's' => GraphicsTransmission::SharedMemory,
            other => GraphicsTransmission::Other(other),
        }
    }
}

impl fmt::Display for GraphicsTransmission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t={}", char::from(self.code()))
    }
}

/// One inline image an application transmitted, as observed on the wire.
///
/// Read the list from [`GraphicsSeen::payloads`]. Every field is a fact the
/// application stated or the wire carried — nothing here is inferred from a
/// rendering, because there is no rendering.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GraphicsPayload {
    protocol: GraphicsProtocol,
    action: GraphicsAction,
    format: GraphicsFormat,
    compressed: bool,
    transmission: Option<GraphicsTransmission>,
    id: Option<u32>,
    size: Option<(u32, u32)>,
    cells: Option<(u16, u16)>,
    chunks: u32,
    bytes: u64,
    at: (u16, u16),
    data: Option<Arc<[u8]>>,
}

impl fmt::Debug for GraphicsPayload {
    /// Compact on purpose: a `Screen` is embedded in every error, and a
    /// derived `Debug` would put a megabyte of base64 into a CI log —
    /// which is how a failure ends up with no diagnosable output at all.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {:?} {}",
            self.protocol,
            self.action,
            match self.size {
                Some((w, h)) => format!("{w}x{h}px"),
                None => "?px".into(),
            }
        )?;
        if let Some((cols, rows)) = self.cells {
            write!(f, " {cols}x{rows}cells")?;
        }
        write!(f, " at {:?}", self.at)?;
        if let Some(transmission) = self.transmission {
            write!(f, " {transmission}")?;
        }
        if let Some(id) = self.id {
            write!(f, " i={id}")?;
        }
        write!(f, " {} {} bytes", self.format, self.bytes)?;
        if self.compressed {
            f.write_str(" zlib")?;
        }
        if self.chunks > 1 {
            write!(f, " in {} chunks", self.chunks)?;
        }
        if self.data.is_none() {
            f.write_str(" (not captured)")?;
        }
        Ok(())
    }
}

impl GraphicsPayload {
    /// Which protocol carried it.
    #[must_use]
    pub fn protocol(&self) -> GraphicsProtocol {
        self.protocol
    }

    /// What the application asked the terminal to do with it.
    #[must_use]
    pub fn action(&self) -> GraphicsAction {
        self.action
    }

    /// How the pixels are encoded.
    #[must_use]
    pub fn format(&self) -> GraphicsFormat {
        self.format
    }

    /// Whether the data is zlib-compressed (kitty `o=z`).
    #[must_use]
    pub fn compressed(&self) -> bool {
        self.compressed
    }

    /// Where the application said this transmission's bytes are (kitty
    /// `t=`), or `None` when it named no medium — which kitty defines as
    /// [`Direct`](GraphicsTransmission::Direct).
    #[must_use]
    pub fn transmission(&self) -> Option<GraphicsTransmission> {
        self.transmission
    }

    /// The image id the application gave it (kitty `i=`), if any.
    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.id
    }

    /// The image's size in pixels, as the application *declared* it — kitty
    /// `s=`/`v=`, or a sixel's raster attributes.
    ///
    /// `None` when nothing declared one, which for sixel means the size is
    /// implicit in the data; `GraphicsPayload::decode` computes it there.
    #[must_use]
    pub fn size(&self) -> Option<(u32, u32)> {
        self.size
    }

    /// The placement the application pinned, in character cells (kitty
    /// `c=`/`r=`).
    ///
    /// This is the field that keeps an image in step with the text around it:
    /// an application that lays out a grid in cells and then transmits an
    /// image of the wrong cell extent draws a picture that slides out from
    /// under its own labels, and nothing on screen says so.
    #[must_use]
    pub fn cells(&self) -> Option<(u16, u16)> {
        self.cells
    }

    /// Where the cursor stood when the payload completed, as `(row, col)` —
    /// which for both protocols is the image's top-left corner. A terminal
    /// that speaks the protocol moves the cursor past the image afterwards,
    /// and so does a termlens terminal declared to
    /// ([`graphics`](crate::TerminalBuilder::graphics)); text that scrolls
    /// later leaves this where the image was placed.
    #[must_use]
    pub fn at(&self) -> (u16, u16) {
        self.at
    }

    /// Escapes the transmission was split across. Kitty caps a payload at
    /// 4096 bytes and continues with `m=1`, so any image of consequence
    /// arrives in several; they are one payload here.
    #[must_use]
    pub fn chunks(&self) -> u32 {
        self.chunks
    }

    /// Bytes this payload occupied on the wire, summed over its chunks:
    /// everything between introducer and terminator, control blocks
    /// included, since that is what the application actually spent.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The image data as it arrived: the base64 body for kitty with the
    /// control blocks stripped and the chunks joined, and everything after
    /// the `q` that closes the header for sixel.
    ///
    /// `None` when the payload fell past the capture bound — see
    /// [`TerminalBuilder::capture_graphics`](crate::TerminalBuilder::capture_graphics).
    /// Deliberately distinct from `Some(&[])`, which is a real transmission
    /// of nothing.
    #[must_use]
    pub fn data(&self) -> Option<&[u8]> {
        self.data.as_deref()
    }

    /// Decode the payload into pixels.
    ///
    /// Supports kitty `f=24`/`f=32`, zlib-compressed or not, and the sixel
    /// data stream. Everything else — `f=100` (PNG), an action that carries
    /// no image, a payload past the capture bound — is an [error naming the
    /// reason](DecodeError) rather than a `None` a test could mistake for
    /// "the image was empty".
    ///
    /// Decoding is done here, on demand, and never as the bytes arrive: a
    /// test that only counts payloads should not pay for inflating them.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError::NotCaptured`] when the payload exceeded the
    /// capture bound, [`DecodeError::NoImage`] when the action carries no
    /// image, and [`DecodeError::Unsupported`] when its format is not
    /// supported. [`DecodeError::Malformed`] covers contradictory or invalid
    /// payload data, including data that cannot be decoded within its declared
    /// size. [`DecodeError::TooLarge`] covers declared dimensions or sixel
    /// commands beyond the 4096-pixel and related safety limits.
    #[cfg(feature = "decode")]
    #[cfg_attr(docsrs, doc(cfg(feature = "decode")))]
    pub fn decode(&self) -> Result<Bitmap, DecodeError> {
        let data = self.data.as_deref().ok_or(DecodeError::NotCaptured)?;
        match self.protocol {
            GraphicsProtocol::Kitty => self.decode_kitty(data),
            GraphicsProtocol::Sixel => decode_sixel(data),
        }
    }

    #[cfg(feature = "decode")]
    fn decode_kitty(&self, data: &[u8]) -> Result<Bitmap, DecodeError> {
        // Only a direct transmission carries pixels. The others name a
        // path or a shared-memory object, so decoding the body decodes the
        // *name* — which is how a payload once reported the ASCII of `/tmp`
        // as an image (#402). An unknown medium is refused for the same
        // reason `f=` does: guessing is what produced that bug.
        if let Some(reason) = match self.transmission {
            None | Some(GraphicsTransmission::Direct) => None,
            Some(GraphicsTransmission::File) => Some("kitty t=f (file transmission)"),
            Some(GraphicsTransmission::TempFile) => Some("kitty t=t (temporary-file transmission)"),
            Some(GraphicsTransmission::SharedMemory) => {
                Some("kitty t=s (shared-memory transmission)")
            }
            Some(GraphicsTransmission::Other(_)) => Some("an unknown kitty t= transmission"),
        } {
            return Err(DecodeError::Unsupported(reason));
        }
        let (channels, has_alpha) = match self.format {
            GraphicsFormat::Rgb => (3usize, false),
            GraphicsFormat::Rgba => (4usize, true),
            GraphicsFormat::Png => return Err(DecodeError::Unsupported("kitty f=100 (PNG)")),
            other => {
                return Err(DecodeError::Unsupported(match other {
                    GraphicsFormat::Sixel => "a sixel stream sent as kitty data",
                    _ => "an unknown kitty f= format",
                }))
            }
        };
        if !self.action.carries_image() {
            return Err(DecodeError::NoImage(self.action));
        }
        let (width, height) = self.size.ok_or(DecodeError::Malformed(
            "a kitty transmission without s= and v=",
        ))?;
        if width == 0 || height == 0 {
            return Err(DecodeError::Malformed(
                "a kitty transmission declaring a zero dimension",
            ));
        }
        // The declared size is computed *before* the payload is touched,
        // because it is what bounds the inflate below. The other way round —
        // as this was — leaves `decompress_to_vec_zlib` free to allocate
        // whatever the compressed bytes expand to, and zlib reaches about
        // 1000:1, so a payload inside the capture bound could ask for
        // gigabytes.
        if width as usize > MAX_DECODED_WIDTH || height as usize > MAX_DECODED_HEIGHT {
            return Err(DecodeError::TooLarge(
                "a kitty transmission declaring more than 4096x4096",
            ));
        }
        let wanted = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(channels))
            .ok_or(DecodeError::Malformed("a declared size that overflows"))?;
        let raw = crate::emu::decode_base64(data).ok_or(DecodeError::Malformed("bad base64"))?;
        let raw = if self.compressed {
            // Nothing past the declared size is ever read, so nothing past it
            // needs inflating.
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&raw, wanted).map_err(|_| {
                DecodeError::Malformed("zlib data that would not inflate within the declared size")
            })?
        } else {
            raw
        };
        if raw.len() < wanted {
            return Err(DecodeError::Malformed(
                "fewer bytes than the declared size needs",
            ));
        }
        let mut pixels = Vec::with_capacity(wanted / channels);
        for chunk in raw[..wanted].chunks_exact(channels) {
            pixels.push([
                chunk[0],
                chunk[1],
                chunk[2],
                if has_alpha { chunk[3] } else { 0xff },
            ]);
        }
        Ok(Bitmap {
            width,
            height,
            pixels,
        })
    }

    pub(crate) fn place(&mut self, at: (u16, u16)) {
        self.at = at;
    }
}

/// The pixels one payload depicted.
///
/// A decoded image, not a rendering: termlens never composites this onto the
/// screen grid, and the grid never mentions it. It exists so an assertion can
/// be about the picture — "the day at week 30 is Primer's brightest green" —
/// rather than about its size in bytes.
#[cfg(feature = "decode")]
#[cfg_attr(docsrs, doc(cfg(feature = "decode")))]
#[derive(Clone, PartialEq, Eq)]
pub struct Bitmap {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 4]>,
}

#[cfg(feature = "decode")]
impl fmt::Debug for Bitmap {
    /// The dimensions, never the pixels: a screen's worth of them in a
    /// timeout error is a log nobody can read.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bitmap {}x{}", self.width, self.height)
    }
}

#[cfg(feature = "decode")]
impl Bitmap {
    /// Width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The pixel at `(x, y)` as RGBA, or `None` when out of bounds.
    ///
    /// Alpha is `0` for a sixel pixel no colour was ever written to — sixel
    /// has no alpha channel, so "transparent" there means "left as the
    /// terminal found it", which is exactly the distinction an assertion
    /// about a rounded corner needs.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = (y as usize) * (self.width as usize) + (x as usize);
        self.pixels.get(index).copied()
    }

    /// Every distinct colour in the image, with how many pixels carry it,
    /// most common first.
    ///
    /// The shape of most assertions about a chart: how many greens, and is
    /// the brightest one the one the palette names.
    ///
    /// **Ties are broken by first appearance**, in raster order (left to
    /// right, top to bottom), so the result is fully determined by the image
    /// and `colours()[0]` has one answer — a test harness must not hand back
    /// a different order on a different run. Linear in the pixel count: a
    /// 512x512 screenshot costs milliseconds, not the seconds a per-pixel
    /// scan of the distinct set used to.
    #[must_use]
    pub fn colours(&self) -> Vec<([u8; 4], u32)> {
        use std::collections::HashMap;

        // One pass, remembering where each colour first appeared so that
        // equal counts have an order the caller can rely on.
        let mut counts: HashMap<[u8; 4], (u32, usize)> = HashMap::new();
        for (index, pixel) in self.pixels.iter().enumerate() {
            counts
                .entry(*pixel)
                .and_modify(|(count, _)| *count += 1)
                .or_insert((1, index));
        }
        let mut seen: Vec<([u8; 4], u32, usize)> = counts
            .into_iter()
            .map(|(colour, (count, first))| (colour, count, first))
            .collect();
        // Count descending, then first appearance: a total order, so the
        // sort's own stability is not what the determinism rests on.
        seen.sort_unstable_by_key(|&(_, count, first)| (std::cmp::Reverse(count), first));
        seen.into_iter()
            .map(|(colour, count, _)| (colour, count))
            .collect()
    }
}

/// The largest image [`GraphicsPayload::decode`] will build.
///
/// Every size in a payload is chosen by the program under test: kitty
/// declares `s=`/`v=` and may arrive zlib-compressed, and sixel declares its
/// raster attributes, names colour registers by index, and repeats a byte
/// `!n` times. Each of those turns a handful of bytes into a request for
/// tens of gigabytes if it is trusted — `!4294967295~` is twelve bytes, and
/// a declared `65535x65535` is about twenty.
///
/// 4096x4096 is far beyond what any terminal can place, and 64 MiB of RGBA
/// once built, so a real image is never refused by it.
#[cfg(feature = "decode")]
const MAX_DECODED_WIDTH: usize = 4096;
/// See [`MAX_DECODED_WIDTH`].
#[cfg(feature = "decode")]
const MAX_DECODED_HEIGHT: usize = 4096;
/// Colour registers a sixel may name. `#n` gives the index directly, so it
/// is attacker-chosen; the common maximum is 256 and no terminal goes past
/// this.
#[cfg(feature = "decode")]
const MAX_SIXEL_REGISTERS: usize = 65_536;

/// Why a payload could not be decoded.
#[cfg(feature = "decode")]
#[cfg_attr(docsrs, doc(cfg(feature = "decode")))]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The payload fell past the capture bound, so its bytes were counted
    /// but not kept. Raise the bound with
    /// [`TerminalBuilder::capture_graphics`](crate::TerminalBuilder::capture_graphics).
    NotCaptured,
    /// The action carries no image at all — a delete, or a bare placement of
    /// something transmitted earlier.
    NoImage(GraphicsAction),
    /// A well-formed payload in an encoding termlens does not decode.
    Unsupported(&'static str),
    /// The payload contradicts itself or the protocol.
    Malformed(&'static str),
    /// The payload asks for more memory than termlens will spend on one
    /// image. Not a judgement about the picture: it is a refusal to let a
    /// size the program under test chose decide how much this allocates.
    TooLarge(&'static str),
}

#[cfg(feature = "decode")]
impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::NotCaptured => f.write_str(
                "the payload was counted but not kept — raise TerminalBuilder::capture_graphics",
            ),
            DecodeError::NoImage(action) => {
                write!(f, "a {action:?} action carries no image data")
            }
            DecodeError::Unsupported(what) => write!(f, "termlens does not decode {what}"),
            DecodeError::Malformed(what) => write!(f, "the payload carries {what}"),
            DecodeError::TooLarge(what) => write!(f, "termlens will not decode {what}"),
        }
    }
}

#[cfg(feature = "decode")]
impl std::error::Error for DecodeError {}

/// Decode a sixel data stream — everything after the `q` that closes the
/// DCS header — into pixels.
///
/// The format in one paragraph: optional raster attributes
/// (`"Pan;Pad;Ph;Pv`) declare the size; `#n;2;r;g;b` defines colour register
/// `n` in percent; `#n` selects it; a byte `?`..`~` paints the six pixels of
/// the current band whose bits it sets; `!<count>` repeats the next such
/// byte; `$` returns to the left margin within the band; `-` ends the band.
#[cfg(feature = "decode")]
fn decode_sixel(data: &[u8]) -> Result<Bitmap, DecodeError> {
    /// A colour register, or `None` where the application never defined one.
    type Registers = Vec<Option<[u8; 4]>>;

    fn percent(value: u32) -> u8 {
        // Sixel colour components are percentages, and the rounding has to
        // match what a terminal does or every assertion lands one off.
        ((value.min(100) * 255 + 50) / 100) as u8
    }

    let mut at = 0usize;
    let mut declared: Option<(u32, u32)> = None;
    let mut registers: Registers = vec![None; 256];
    let mut current = 0usize;
    // Rows of runs, grown as the data addresses them: a sixel need not
    // declare its size, and one that does can still overrun it.
    let mut rows: Vec<Vec<Option<[u8; 4]>>> = Vec::new();
    let mut band_top = 0usize;
    let mut x = 0usize;
    let mut width_seen = 0usize;

    /// Read a `;`-separated run of decimal parameters.
    fn params(data: &[u8], at: &mut usize) -> Vec<u32> {
        let mut out = vec![0u32];
        while *at < data.len() {
            match data[*at] {
                b'0'..=b'9' => {
                    let last = out.last_mut().expect("seeded with one parameter");
                    *last = last
                        .saturating_mul(10)
                        .saturating_add(u32::from(data[*at] - b'0'));
                }
                b';' => out.push(0),
                _ => break,
            }
            *at += 1;
        }
        out
    }

    while at < data.len() {
        match data[at] {
            b'"' => {
                at += 1;
                let raster = params(data, &mut at);
                if let (Some(&width), Some(&height)) = (raster.get(2), raster.get(3)) {
                    declared = Some((width, height));
                }
            }
            b'#' => {
                at += 1;
                let values = params(data, &mut at);
                let index = values.first().copied().unwrap_or(0) as usize;
                if index >= MAX_SIXEL_REGISTERS {
                    return Err(DecodeError::TooLarge(
                        "a sixel colour register index past 65536",
                    ));
                }
                if index >= registers.len() {
                    registers.resize(index + 1, None);
                }
                if values.len() >= 5 {
                    // `#n;1;…` is HLS; only RGB (`2`) is defined here, and a
                    // guessed conversion would put wrong colours into an
                    // assertion that reads as exact.
                    if values[1] != 2 {
                        return Err(DecodeError::Unsupported("sixel HLS colours"));
                    }
                    registers[index] = Some([
                        percent(values[2]),
                        percent(values[3]),
                        percent(values[4]),
                        0xff,
                    ]);
                }
                current = index;
            }
            b'!' => {
                at += 1;
                let counts = params(data, &mut at);
                let count = counts.first().copied().unwrap_or(0) as usize;
                if at < data.len() && (0x3f..=0x7e).contains(&data[at]) {
                    let bits = data[at] - 0x3f;
                    at += 1;
                    paint(
                        &mut rows,
                        &mut width_seen,
                        band_top,
                        &mut x,
                        bits,
                        count,
                        registers.get(current).copied().flatten(),
                    )?;
                }
            }
            0x3f..=0x7e => {
                let bits = data[at] - 0x3f;
                at += 1;
                paint(
                    &mut rows,
                    &mut width_seen,
                    band_top,
                    &mut x,
                    bits,
                    1,
                    registers.get(current).copied().flatten(),
                )?;
            }
            b'$' => {
                at += 1;
                x = 0;
            }
            b'-' => {
                at += 1;
                band_top += 6;
                x = 0;
            }
            // Whitespace between records, and anything else the stream
            // carries that paints nothing.
            _ => at += 1,
        }
    }

    fn paint(
        rows: &mut Vec<Vec<Option<[u8; 4]>>>,
        width_seen: &mut usize,
        band_top: usize,
        x: &mut usize,
        bits: u8,
        count: usize,
        colour: Option<[u8; 4]>,
    ) -> Result<(), DecodeError> {
        for _ in 0..count {
            // Checked every step rather than once against `count`: `!n` gives
            // a `u32`, and the cursor also carries over from earlier runs in
            // the same band. Outside the colour branch because an undefined
            // register still advances the cursor, so a huge repeat would
            // otherwise spin four billion times painting nothing.
            if *x >= MAX_DECODED_WIDTH {
                return Err(DecodeError::TooLarge("a sixel wider than 4096 pixels"));
            }
            if let Some(colour) = colour {
                for bit in 0..6 {
                    if bits & (1 << bit) != 0 {
                        let y = band_top + bit;
                        if y >= MAX_DECODED_HEIGHT {
                            return Err(DecodeError::TooLarge("a sixel taller than 4096 pixels"));
                        }
                        if rows.len() <= y {
                            rows.resize(y + 1, Vec::new());
                        }
                        let row = &mut rows[y];
                        if row.len() <= *x {
                            row.resize(*x + 1, None);
                        }
                        row[*x] = Some(colour);
                    }
                }
            }
            *x += 1;
            *width_seen = (*width_seen).max(*x);
        }
        Ok(())
    }

    let (width, height) = match declared {
        Some((width, height)) if width > 0 && height > 0 => (width, height),
        _ => (width_seen as u32, rows.len() as u32),
    };
    // The declared raster attributes reach this allocation without the pixel
    // data being touched at all, so a payload declaring 65535x65535 and
    // painting nothing still asks for about 17 GB.
    if width as usize > MAX_DECODED_WIDTH || height as usize > MAX_DECODED_HEIGHT {
        return Err(DecodeError::TooLarge(
            "a sixel declaring more than 4096x4096",
        ));
    }
    let mut pixels = Vec::with_capacity((width as usize).saturating_mul(height as usize));
    for y in 0..height as usize {
        for x in 0..width as usize {
            pixels.push(
                rows.get(y)
                    .and_then(|row| row.get(x))
                    .copied()
                    .flatten()
                    // Untouched: sixel has no alpha, so a pixel never
                    // painted is the terminal's own background showing
                    // through, which is not a colour we may invent.
                    .unwrap_or([0, 0, 0, 0]),
            );
        }
    }
    Ok(Bitmap {
        width,
        height,
        pixels,
    })
}

/// Inline graphics payloads the application transmitted, as observed at one
/// snapshot.
///
/// Read it from a [`Screen`](crate::Screen) via
/// [`Screen::graphics`](crate::Screen::graphics). The counters are
/// cumulative and monotonic, so a test takes a delta around an action rather
/// than resetting a gauge; [`payloads`](Self::payloads) is the bounded tail
/// of what was captured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GraphicsSeen {
    pub(crate) counts: GraphicsCounts,
    pub(crate) payloads: Arc<Vec<GraphicsPayload>>,
}

impl GraphicsSeen {
    pub(crate) fn new(counts: GraphicsCounts, payloads: Arc<Vec<GraphicsPayload>>) -> Self {
        Self { counts, payloads }
    }
}

/// The cumulative counters, kept by the sequence tracker as bytes arrive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub(crate) struct GraphicsCounts {
    pub(crate) kitty: u32,
    pub(crate) sixel: u32,
    pub(crate) deletes: u32,
    pub(crate) bytes: u64,
}

impl GraphicsCounts {
    /// Count one completed payload. Bytes are counted for every action —
    /// a delete costs the wire something too — but only an action that
    /// carries a picture counts as an image.
    pub(crate) fn record(&mut self, payload: &GraphicsPayload) {
        self.bytes += payload.bytes();
        if payload.action() == GraphicsAction::Delete {
            self.deletes += 1;
        }
        if !payload.action().carries_image() {
            return;
        }
        match payload.protocol() {
            GraphicsProtocol::Kitty => self.kitty += 1,
            GraphicsProtocol::Sixel => self.sixel += 1,
        }
    }
}

impl GraphicsSeen {
    /// Kitty images transmitted (`APC G … ST`).
    ///
    /// **Images, not escapes.** A transmission split across the protocol's
    /// 4096-byte chunks is one image, and an action carrying no picture — a
    /// delete above all — is not one at all. Nor is an upload abandoned
    /// before its last chunk: a delete aborts one, as the protocol requires,
    /// and so does a sixel or a hard reset (`RIS`) arriving in the middle.
    #[must_use]
    pub fn kitty(&self) -> u32 {
        self.counts.kitty
    }

    /// Sixel images transmitted (`DCS q … ST`).
    #[must_use]
    pub fn sixel(&self) -> u32 {
        self.counts.sixel
    }

    /// Images of either protocol.
    #[must_use]
    pub fn total(&self) -> u32 {
        self.counts.kitty + self.counts.sixel
    }

    /// Kitty delete commands (`a=d`) — images taken *off* the screen.
    ///
    /// Counted apart from [`total`](Self::total) because a delete carries no
    /// picture: an application that tears down what it drew and one that
    /// draws twice as much are opposite behaviours, and folding them
    /// together made the difference invisible.
    #[must_use]
    pub fn deletes(&self) -> u32 {
        self.counts.deletes
    }

    /// True when the application has transmitted no inline graphics at all.
    ///
    /// Deletes do not count: an application whose only graphics traffic is a
    /// teardown has drawn nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// Total payload bytes across both protocols and every action, counted
    /// the same way for each: everything between the introducer and the
    /// terminator. The chunks of an abandoned upload count too, since they
    /// crossed the wire.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.counts.bytes
    }

    /// The payloads themselves, oldest first — what went out, where it was
    /// placed, and, with the `decode` feature, what it depicted.
    ///
    /// Bounded, like scrollback: the most recent payloads within the capture
    /// bound, which
    /// [`TerminalBuilder::capture_graphics`](crate::TerminalBuilder::capture_graphics)
    /// sets. The counters above stay truthful whatever the bound, so a test
    /// that only counts is never affected by one.
    #[must_use]
    pub fn payloads(&self) -> &[GraphicsPayload] {
        &self.payloads
    }

    /// The most recent payload, if any.
    #[must_use]
    pub fn last(&self) -> Option<&GraphicsPayload> {
        self.payloads.last()
    }

    /// Counters without payloads, for tests that assemble a `TermState` by
    /// hand rather than driving an emulator.
    #[cfg(test)]
    pub(crate) fn for_test(kitty: u32, sixel: u32, deletes: u32, bytes: u64) -> Self {
        Self {
            counts: GraphicsCounts {
                kitty,
                sixel,
                deletes,
                bytes,
            },
            payloads: Arc::new(Vec::new()),
        }
    }
}

/// Where an image lands, as far as its escape says: what the emulator needs
/// to move the cursor past it the way the terminal it simulates does (#588).
///
/// Kept beside the payload rather than in it: the payload's fields are its
/// JSON, which a saved screen promises not to change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Placement {
    /// kitty's `c=`, `r=`: the placement's size in cells, 0 when the escape
    /// leaves it to the image's size, as kitty reads a missing key.
    pub(crate) cols: u32,
    pub(crate) rows: u32,
    /// kitty's `w=`, `h=`, `x=`, `y=`: the source rectangle within the image,
    /// 0 for the whole image.
    pub(crate) src_width: u32,
    pub(crate) src_height: u32,
    pub(crate) src_x: u32,
    pub(crate) src_y: u32,
    /// kitty's `X=`, `Y=`: the offset within the first cell, in pixels.
    pub(crate) cell_x: u32,
    pub(crate) cell_y: u32,
    /// kitty: whether a placement moves the cursor at all. `C=1` asks it not
    /// to, and neither a Unicode-placeholder placement (`U=1`) nor one
    /// relative to another image (`P=`) moves it.
    pub(crate) moves: bool,
    /// sixel: the image's height in pixels as xterm measures it, the larger
    /// of the raster attributes' and the painted rows'.
    pub(crate) height: u32,
}

/// A payload under construction, owned by the sequence tracker: kitty splits
/// one image across escapes, so a payload is not complete until a kitty
/// escape arrives without `m=1` — and it is [abandoned](Self::abandon)
/// instead if a delete, a sixel or a hard reset arrives first.
#[derive(Debug)]
pub(crate) struct GraphicsBuilder {
    protocol: Option<GraphicsProtocol>,
    action: GraphicsAction,
    format: GraphicsFormat,
    compressed: bool,
    transmission: Option<GraphicsTransmission>,
    id: Option<u32>,
    size: Option<(u32, u32)>,
    cells: Option<(u16, u16)>,
    chunks: u32,
    bytes: u64,
    data: Vec<u8>,
    /// Set once any chunk arrived past the capture bound. The payload then
    /// reports no data at all rather than a prefix of one.
    dropped: bool,
    placement: Placement,
}

impl Default for GraphicsBuilder {
    fn default() -> Self {
        Self {
            protocol: None,
            action: GraphicsAction::Other,
            format: GraphicsFormat::Rgba,
            compressed: false,
            transmission: None,
            id: None,
            size: None,
            cells: None,
            chunks: 0,
            bytes: 0,
            data: Vec::new(),
            dropped: false,
            placement: Placement::default(),
        }
    }
}

impl GraphicsBuilder {
    /// Take on the facts of a kitty control block. Only the first escape of
    /// a chunked transmission carries one; the continuations carry `m=` and
    /// nothing else, so nothing here overwrites what is already known.
    pub(crate) fn kitty(&mut self, control: &[u8]) {
        if self.protocol.is_none() {
            self.protocol = Some(GraphicsProtocol::Kitty);
            self.action = match key(control, b"a") {
                // `a=t`, transmit only, is the protocol's default: a
                // transmission that names no action shows nothing (#588).
                Some(b"t") | None => GraphicsAction::Transmit,
                Some(b"p") => GraphicsAction::Place,
                Some(b"T") => GraphicsAction::TransmitAndPlace,
                Some(b"d") => GraphicsAction::Delete,
                Some(_) => GraphicsAction::Other,
            };
            self.format = match number(control, b"f") {
                Some(24) => GraphicsFormat::Rgb,
                // 32 is the protocol's default.
                Some(32) | None => GraphicsFormat::Rgba,
                Some(100) => GraphicsFormat::Png,
                Some(other) => GraphicsFormat::Other(other),
            };
            self.compressed = key(control, b"o") == Some(b"z");
            self.transmission = key(control, b"t")
                .and_then(|value| value.first().copied())
                .map(GraphicsTransmission::from_code);
            self.id = number(control, b"i");
            self.size = match (number(control, b"s"), number(control, b"v")) {
                (Some(width), Some(height)) => Some((width, height)),
                _ => None,
            };
            self.cells = match (number(control, b"c"), number(control, b"r")) {
                (Some(cols), Some(rows)) => Some((
                    cols.min(u32::from(u16::MAX)) as u16,
                    rows.min(u32::from(u16::MAX)) as u16,
                )),
                _ => None,
            };
            let value = |name: &[u8]| number(control, name).unwrap_or(0);
            self.placement = Placement {
                cols: value(b"c"),
                rows: value(b"r"),
                src_width: value(b"w"),
                src_height: value(b"h"),
                src_x: value(b"x"),
                src_y: value(b"y"),
                cell_x: value(b"X"),
                cell_y: value(b"Y"),
                moves: value(b"C") != 1 && value(b"U") != 1 && key(control, b"P").is_none(),
                height: 0,
            };
        }
    }

    /// Take on the facts of a sixel header: the raster attributes are inside
    /// the data, so only the protocol is settled here. Nothing else is
    /// reset, so the builder must hold no kitty upload — the tracker
    /// abandons one first.
    pub(crate) fn sixel(&mut self) {
        self.protocol = Some(GraphicsProtocol::Sixel);
        self.action = GraphicsAction::TransmitAndPlace;
        self.format = GraphicsFormat::Sixel;
    }

    /// Add one escape's worth of wire cost and image data.
    ///
    /// `complete` is false when the capture bound cut this chunk short, and
    /// `cap` bounds the payload as a whole — a kitty transmission arrives as
    /// one escape per 4096 bytes, so a bound applied per escape would let a
    /// thousand-chunk image retain a thousand times the budget.
    ///
    /// The counts stay exact either way; past the bound the data is dropped
    /// entirely rather than kept as a prefix that would decode into a
    /// plausible-looking wrong picture.
    pub(crate) fn chunk(&mut self, bytes: u64, data: &[u8], complete: bool, cap: usize) {
        // Measured here, before a chunk past the capture bound is dropped:
        // the cursor moves past an image whatever this test chose to keep.
        if self.protocol == Some(GraphicsProtocol::Sixel) && self.chunks == 0 {
            let declared = raster_size(data).map_or(0, |(_, height)| height);
            self.placement.height = declared.max(sixel_painted_height(data));
        }
        self.chunks += 1;
        self.bytes += bytes;
        let fits = self.data.len().saturating_add(data.len()) <= cap;
        if complete && fits && !self.dropped {
            self.data.extend_from_slice(data);
        } else {
            self.dropped = true;
            self.data = Vec::new();
        }
    }

    /// [`finish_placed`](Self::finish_placed) without the placement.
    #[cfg(test)]
    pub(crate) fn finish(&mut self) -> Option<GraphicsPayload> {
        self.finish_placed().map(|(payload, _)| payload)
    }

    /// Finish the payload, with where it lands. `at` is stamped later, by
    /// the emulator, once the grid has caught up with the terminator.
    pub(crate) fn finish_placed(&mut self) -> Option<(GraphicsPayload, Placement)> {
        let protocol = self.protocol.take()?;
        let placement = self.placement;
        let mut size = self.size;
        if protocol == GraphicsProtocol::Sixel {
            size = raster_size(&self.data).or(size);
        }
        let payload = GraphicsPayload {
            protocol,
            action: self.action,
            format: self.format,
            compressed: self.compressed,
            transmission: self.transmission,
            id: self.id,
            size,
            cells: self.cells,
            chunks: self.chunks,
            bytes: self.bytes,
            at: (0, 0),
            data: (!self.dropped)
                .then(|| Arc::from(std::mem::take(&mut self.data).into_boxed_slice())),
        };
        *self = Self::default();
        Some((payload, placement))
    }

    /// Drop a transmission that will never complete, handing back the wire
    /// bytes its chunks already cost. Nothing of it becomes a payload: the
    /// protocol aborts the upload, so there is no image to report. The bytes
    /// are another matter — they crossed the wire, and
    /// [`GraphicsSeen::bytes`] counts traffic, not pictures.
    pub(crate) fn abandon(&mut self) -> u64 {
        let spent = self.bytes;
        *self = Self::default();
        spent
    }
}

/// The `"Pan;Pad;Ph;Pv` raster attributes at the head of a sixel stream, if
/// it declares any.
fn raster_size(data: &[u8]) -> Option<(u32, u32)> {
    let at = data.iter().position(|&b| b == b'"')?;
    // Only a leading raster record describes the whole image; one appearing
    // after pixels have been painted is a different statement.
    if data[..at].iter().any(|b| (0x3f..=0x7e).contains(b)) {
        return None;
    }
    let mut values = vec![0u32];
    for &b in &data[at + 1..] {
        match b {
            b'0'..=b'9' => {
                let last = values.last_mut().expect("seeded with one parameter");
                *last = last.saturating_mul(10).saturating_add(u32::from(b - b'0'));
            }
            b';' => values.push(0),
            _ => break,
        }
    }
    match (values.get(2), values.get(3)) {
        (Some(&width), Some(&height)) if width > 0 && height > 0 => Some((width, height)),
        _ => None,
    }
}

/// The height in pixels the sixels in `data` paint, as xterm's decoder
/// measures it: down to the lowest pixel set. Bands of six rows are
/// separated by `-`, and each sixel character's bits are its band's rows,
/// the lowest bit the top one.
fn sixel_painted_height(data: &[u8]) -> u32 {
    let mut band = 0u32;
    let mut height = 0u32;
    for &byte in data {
        match byte {
            b'-' => band = band.saturating_add(1),
            0x3f..=0x7e => {
                let bits = u32::from(byte - 0x3f);
                if bits != 0 {
                    let lowest = band.saturating_mul(6) + (u32::BITS - bits.leading_zeros());
                    height = height.max(lowest);
                }
            }
            _ => {}
        }
    }
    height
}

/// The cells a kitty placement covers, `(cols, rows)`, computed as kitty
/// computes them (`update_dest_rect` in kitty's `graphics.c`): `c=` and `r=`
/// when both are given; otherwise from the source rectangle — `w=`/`h=` or
/// the whole image, less `x=`/`y=` — and the cell size, keeping the image's
/// aspect when one of the two is given. `None` when that needs an image size
/// or a cell size nobody stated.
pub(crate) fn kitty_cells(
    placement: &Placement,
    image: Option<(u32, u32)>,
    cell: Option<(u16, u16)>,
) -> Option<(u32, u32)> {
    let (mut cols, mut rows) = (placement.cols, placement.rows);
    if cols > 0 && rows > 0 {
        return Some((cols, rows));
    }
    let (cell_w, cell_h) = cell
        .map(|(w, h)| (u32::from(w), u32::from(h)))
        .filter(|&(w, h)| w > 0 && h > 0)?;
    let (image_w, image_h) = image?;
    let src_w = if placement.src_width > 0 {
        placement.src_width
    } else {
        image_w
    };
    let src_h = if placement.src_height > 0 {
        placement.src_height
    } else {
        image_h
    };
    let src_w = src_w.min(image_w - placement.src_x.min(image_w));
    let src_h = src_h.min(image_h - placement.src_y.min(image_h));
    let offset_x = placement.cell_x.min(cell_w - 1);
    let offset_y = placement.cell_y.min(cell_h - 1);
    let (auto_cols, auto_rows) = (cols == 0, rows == 0);
    if auto_cols {
        cols = if auto_rows {
            (src_w + offset_x).div_ceil(cell_w)
        } else if src_h == 0 {
            0
        } else {
            let height = f64::from(cell_h * rows + offset_y);
            (height * f64::from(src_w) / f64::from(src_h) / f64::from(cell_w)).ceil() as u32
        };
    }
    if auto_rows {
        rows = if auto_cols {
            (src_h + offset_y).div_ceil(cell_h)
        } else if src_w == 0 {
            0
        } else {
            let width = f64::from(cell_w * cols + offset_x);
            (width * f64::from(src_h) / f64::from(src_w) / f64::from(cell_h)).ceil() as u32
        };
    }
    Some((cols, rows))
}

/// The raw value of `name` in a kitty control block (`a=T,i=3,f=32`).
fn key<'a>(control: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    control.split(|&b| b == b',').find_map(|pair| {
        let (found, value) = split_once(pair, b'=')?;
        (found == name).then_some(value)
    })
}

/// The numeric value of `name`, if it carries one.
fn number(control: &[u8], name: &[u8]) -> Option<u32> {
    std::str::from_utf8(key(control, name)?).ok()?.parse().ok()
}

fn split_once(bytes: &[u8], separator: u8) -> Option<(&[u8], &[u8])> {
    let at = bytes.iter().position(|&b| b == separator)?;
    Some((&bytes[..at], &bytes[at + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kitty_payload(control: &[u8], data: &[u8]) -> GraphicsPayload {
        let mut builder = GraphicsBuilder::default();
        builder.kitty(control);
        builder.chunk(data.len() as u64, data, true, usize::MAX);
        builder.finish().expect("a payload")
    }

    #[test]
    fn a_kitty_control_block_yields_every_fact_it_states() {
        let payload = kitty_payload(b"a=T,q=2,f=32,o=z,s=954,v=133,i=7,c=106,r=7", b"AAAA");
        assert_eq!(payload.protocol(), GraphicsProtocol::Kitty);
        assert_eq!(payload.action(), GraphicsAction::TransmitAndPlace);
        assert_eq!(payload.format(), GraphicsFormat::Rgba);
        assert!(payload.compressed());
        assert_eq!(payload.transmission(), None);
        assert_eq!(payload.id(), Some(7));
        assert_eq!(payload.size(), Some((954, 133)));
        assert_eq!(payload.cells(), Some((106, 7)));
        assert_eq!(payload.data(), Some(&b"AAAA"[..]));
    }

    #[test]
    fn the_protocols_defaults_are_the_protocols_defaults() {
        // A transmission naming neither action nor format is `a=t,f=32`:
        // transmit only. It was reported as `a=T`, an image placed, which
        // kitty never shows (#588).
        let payload = kitty_payload(b"s=1,v=1", b"AAAA");
        assert_eq!(payload.action(), GraphicsAction::Transmit);
        assert_eq!(payload.format(), GraphicsFormat::Rgba);
        assert!(!payload.compressed());
    }

    /// The placement facts the cursor move needs, read off the control
    /// block of the first chunk (#588).
    #[test]
    fn a_kitty_placement_records_its_size_source_and_whether_it_moves() {
        let mut builder = GraphicsBuilder::default();
        builder.kitty(b"a=T,f=24,s=8,v=4,c=3,w=6,h=2,x=1,y=1,X=4,Y=5");
        builder.chunk(4, b"AAAA", true, usize::MAX);
        let (_, placement) = builder.finish_placed().expect("a payload");
        assert_eq!(
            placement,
            Placement {
                cols: 3,
                rows: 0,
                src_width: 6,
                src_height: 2,
                src_x: 1,
                src_y: 1,
                cell_x: 4,
                cell_y: 5,
                moves: true,
                height: 0,
            }
        );
        for control in [&b"a=T,C=1"[..], b"a=T,U=1", b"a=p,i=1,P=2"] {
            let mut builder = GraphicsBuilder::default();
            builder.kitty(control);
            builder.chunk(0, b"", true, usize::MAX);
            let (_, placement) = builder.finish_placed().expect("a payload");
            assert!(!placement.moves, "{}", String::from_utf8_lossy(control));
        }
    }

    /// kitty's own arithmetic (`update_dest_rect`): the cells given, or the
    /// source rectangle over the cell size, rounded up, keeping the aspect
    /// when only one side is given.
    #[test]
    fn kitty_sizes_a_placement_as_kitty_does() {
        let cell = Some((10, 20));
        let given = |cols, rows| Placement {
            cols,
            rows,
            ..Placement::default()
        };
        // Both given: no image or cell size needed.
        assert_eq!(kitty_cells(&given(3, 2), None, None), Some((3, 2)));
        // Neither: the image over the cell, rounded up.
        assert_eq!(
            kitty_cells(&given(0, 0), Some((25, 41)), cell),
            Some((3, 3))
        );
        // One given: the other keeps the aspect. 2 rows are 40 px tall, so a
        // 20x10 image is 80 px wide there: 8 columns.
        assert_eq!(
            kitty_cells(&given(0, 2), Some((20, 10)), cell),
            Some((8, 2))
        );
        assert_eq!(
            kitty_cells(&given(4, 0), Some((20, 10)), cell),
            Some((4, 1))
        );
        // A source rectangle, less its origin, and the offset in the cell.
        let cropped = Placement {
            src_width: 15,
            src_height: 50,
            src_x: 10,
            cell_x: 6,
            ..given(0, 0)
        };
        // 15 wide but only 10 left past x=10; plus 6: 16 px, 2 columns.
        assert_eq!(kitty_cells(&cropped, Some((20, 60)), cell), Some((2, 3)));
        // Nothing to compute from: no move rather than a guess.
        assert_eq!(kitty_cells(&given(0, 0), None, cell), None);
        assert_eq!(kitty_cells(&given(0, 0), Some((20, 10)), None), None);
    }

    /// xterm measures a sixel down to its lowest painted pixel; each sixel
    /// character's bits are its band's six rows, the low bit the top one.
    #[test]
    fn a_sixel_is_as_tall_as_the_lowest_pixel_it_paints() {
        assert_eq!(sixel_painted_height(b"#0~~~~"), 6);
        assert_eq!(sixel_painted_height(b"#0~-~-@"), 13);
        // `?` paints nothing; colour and raster parameters are not sixels.
        assert_eq!(sixel_painted_height(b"\"1;1;4;45#0;2;100;0;0??"), 0);
        // The builder takes the larger of the raster's and the painted one,
        // from the data as it arrives — kept past the capture bound too.
        let mut builder = GraphicsBuilder::default();
        builder.sixel();
        builder.chunk(8, b"\"1;1;4;45#0~-~", true, 0);
        let (payload, placement) = builder.finish_placed().expect("a payload");
        assert_eq!(payload.data(), None, "dropped by the bound");
        assert_eq!(placement.height, 45);
    }

    #[test]
    fn a_delete_is_not_an_image() {
        let payload = kitty_payload(b"a=d,d=I,i=1,q=2", b"");
        assert_eq!(payload.action(), GraphicsAction::Delete);
        assert!(!payload.action().carries_image());
    }

    #[test]
    fn an_unknown_action_is_not_guessed_to_carry_one() {
        let payload = kitty_payload(b"a=z,i=1", b"");
        assert_eq!(payload.action(), GraphicsAction::Other);
        assert!(!payload.action().carries_image());
    }

    #[test]
    fn chunks_join_into_one_payload() {
        let mut builder = GraphicsBuilder::default();
        builder.kitty(b"a=T,f=32,s=2,v=1,m=1");
        builder.chunk(20, b"AAAA", true, usize::MAX);
        builder.chunk(10, b"BBBB", true, usize::MAX);
        let payload = builder.finish().expect("a payload");
        assert_eq!(payload.chunks(), 2);
        assert_eq!(payload.bytes(), 30);
        assert_eq!(payload.data(), Some(&b"AAAABBBB"[..]));
    }

    #[test]
    fn a_payload_past_the_bound_is_counted_and_not_kept() {
        let mut builder = GraphicsBuilder::default();
        builder.kitty(b"a=T,f=32,s=2,v=1");
        builder.chunk(64, b"AAAABBBB", false, usize::MAX);
        let payload = builder.finish().expect("a payload");
        assert_eq!(payload.bytes(), 64, "the cost is still known");
        assert_eq!(payload.data(), None, "and the bytes are not kept");
    }

    #[test]
    fn a_sixel_reads_its_size_off_its_raster_attributes() {
        let mut builder = GraphicsBuilder::default();
        builder.sixel();
        builder.chunk(30, b"\"1;1;18;12#0;2;100;100;100~", true, usize::MAX);
        let payload = builder.finish().expect("a payload");
        assert_eq!(payload.protocol(), GraphicsProtocol::Sixel);
        assert_eq!(payload.size(), Some((18, 12)));
        assert_eq!(payload.format(), GraphicsFormat::Sixel);
    }

    #[test]
    fn a_sixel_that_declares_no_size_says_so_rather_than_guessing() {
        let mut builder = GraphicsBuilder::default();
        builder.sixel();
        builder.chunk(10, b"#0;2;100;100;100~~~", true, usize::MAX);
        assert_eq!(builder.finish().expect("a payload").size(), None);
    }

    #[cfg(feature = "decode")]
    #[test]
    fn a_kitty_rgba_transmission_decodes_to_its_pixels() {
        // Two pixels: opaque red, half-transparent blue.
        let raw = [0xff, 0x00, 0x00, 0xff, 0x00, 0x00, 0xff, 0x80];
        let data = base64(&raw);
        let payload = kitty_payload(b"a=T,f=32,s=2,v=1", data.as_bytes());
        let bitmap = payload.decode().expect("decodes");
        assert_eq!((bitmap.width(), bitmap.height()), (2, 1));
        assert_eq!(bitmap.pixel(0, 0), Some([0xff, 0, 0, 0xff]));
        assert_eq!(bitmap.pixel(1, 0), Some([0, 0, 0xff, 0x80]));
        assert_eq!(bitmap.pixel(2, 0), None, "out of bounds is None");
    }

    #[cfg(feature = "decode")]
    #[test]
    fn an_rgb_transmission_is_opaque() {
        let data = base64(&[0x11, 0x22, 0x33]);
        let payload = kitty_payload(b"a=T,f=24,s=1,v=1", data.as_bytes());
        let bitmap = payload.decode().expect("decodes");
        assert_eq!(bitmap.pixel(0, 0), Some([0x11, 0x22, 0x33, 0xff]));
    }

    #[cfg(feature = "decode")]
    fn sixel_payload(data: &[u8]) -> GraphicsPayload {
        let mut builder = GraphicsBuilder::default();
        builder.sixel();
        builder.chunk(data.len() as u64, data, true, usize::MAX);
        builder.finish().expect("a payload")
    }

    /// Every size in a payload is chosen by the program under test, and each
    /// of these turns a handful of bytes into a request for gigabytes if it
    /// is trusted. All five were live before the ceiling went in.
    ///
    /// One table on purpose, so a sixth path cannot appear without a line
    /// here saying what bounds it.
    #[cfg(feature = "decode")]
    #[test]
    fn a_payload_cannot_choose_how_much_memory_a_decode_spends() {
        // A declared size no terminal could place, refused before any data
        // is read.
        assert!(
            matches!(
                kitty_payload(b"a=T,f=32,s=65535,v=65535", b"AAAA").decode(),
                Err(DecodeError::TooLarge(_))
            ),
            "a 65535x65535 kitty transmission was not refused"
        );

        // A zlib bomb: four declared bytes, four mebibytes inside. The
        // inflate is bounded by the declared size, so it never expands.
        let bomb = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 4 << 20], 9);
        assert!(bomb.len() < 64 << 10, "the fixture is meant to be small");
        assert!(
            matches!(
                kitty_payload(b"a=T,f=32,o=z,s=1,v=1", base64(&bomb).as_bytes()).decode(),
                Err(DecodeError::Malformed(_))
            ),
            "a payload inflating past its declared size was not refused"
        );

        // A sixel naming a colour register far past any palette.
        assert!(
            matches!(
                sixel_payload(b"#4000000000;2;100;100;100~").decode(),
                Err(DecodeError::TooLarge(_))
            ),
            "a four-billion colour register was not refused"
        );

        // A sixel repeat count. Twelve bytes of input.
        assert!(
            matches!(
                sixel_payload(b"#0;2;100;100;100!4294967295~").decode(),
                Err(DecodeError::TooLarge(_))
            ),
            "a four-billion repeat was not refused"
        );

        // Raster attributes, which reach the final allocation without the
        // pixel data being touched at all.
        assert!(
            matches!(
                sixel_payload(b"\"1;1;65535;65535#0;2;100;100;100~").decode(),
                Err(DecodeError::TooLarge(_))
            ),
            "a 65535x65535 declared sixel was not refused"
        );
    }

    /// The ceiling must not refuse anything real: a terminal-sized image in
    /// either protocol decodes exactly as before.
    #[cfg(feature = "decode")]
    #[test]
    fn an_ordinary_image_is_untouched_by_the_ceiling() {
        let raw = vec![0x40u8; 64 * 32 * 4];
        let bitmap = kitty_payload(b"a=T,f=32,s=64,v=32", base64(&raw).as_bytes())
            .decode()
            .expect("a 64x32 image still decodes");
        assert_eq!((bitmap.width(), bitmap.height()), (64, 32));
        assert_eq!(bitmap.pixel(63, 31), Some([0x40, 0x40, 0x40, 0x40]));

        let sixel = sixel_payload(b"\"1;1;4;6#0;2;100;0;0~~~~")
            .decode()
            .expect("a small sixel still decodes");
        assert_eq!((sixel.width(), sixel.height()), (4, 6));
    }

    #[cfg(feature = "decode")]
    #[test]
    fn a_compressed_transmission_is_inflated_first() {
        let raw = vec![0x40u8; 4 * 16 * 16];
        let data = base64(&miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6));
        let payload = kitty_payload(b"a=T,f=32,o=z,s=16,v=16", data.as_bytes());
        let bitmap = payload.decode().expect("decodes");
        assert_eq!((bitmap.width(), bitmap.height()), (16, 16));
        assert_eq!(bitmap.pixel(15, 15), Some([0x40, 0x40, 0x40, 0x40]));
    }

    /// A declared dimension of zero contradicts the protocol — `s=` and `v=`
    /// are the pixel dimensions of a picture — so it is a refusal with a
    /// reason, never an `Ok` of an empty bitmap that reads as "the image was
    /// empty" (#404). Sixel already declines this by falling back to the
    /// painted extent.
    #[cfg(feature = "decode")]
    #[test]
    fn kitty_zero_dimensions_are_malformed() {
        for control in [
            b"a=T,f=32,s=0,v=0".as_slice(),
            b"a=T,f=32,s=8,v=0".as_slice(),
            b"a=T,f=32,s=0,v=8".as_slice(),
        ] {
            assert!(
                matches!(
                    kitty_payload(control, b"").decode(),
                    Err(DecodeError::Malformed(
                        "a kitty transmission declaring a zero dimension"
                    ))
                ),
                "a kitty transmission with a zero dimension was accepted: {}",
                String::from_utf8_lossy(control)
            );
        }
    }

    #[cfg(feature = "decode")]
    #[test]
    fn non_direct_kitty_transmissions_are_not_decoded_as_pixels() {
        let data = base64(&[1, 2, 3, 4]);
        for medium in *b"fts" {
            let control = format!("a=T,t={},f=32,s=1,v=1", char::from(medium));
            let payload = kitty_payload(control.as_bytes(), data.as_bytes());
            assert_eq!(
                payload.transmission(),
                Some(GraphicsTransmission::from_code(medium))
            );
            assert!(matches!(payload.decode(), Err(DecodeError::Unsupported(_))));
        }

        let direct = kitty_payload(b"a=T,t=d,f=32,s=1,v=1", data.as_bytes());
        assert_eq!(direct.transmission(), Some(GraphicsTransmission::Direct));
        assert_eq!(
            direct.decode().expect("direct data decodes").pixel(0, 0),
            Some([1, 2, 3, 4])
        );
    }

    #[cfg(feature = "decode")]
    #[test]
    fn every_refusal_names_its_reason() {
        let png = kitty_payload(b"a=T,f=100,s=1,v=1", b"AAAA");
        assert!(matches!(png.decode(), Err(DecodeError::Unsupported(_))));

        let delete = kitty_payload(b"a=d,i=1", b"");
        assert!(matches!(
            delete.decode(),
            Err(DecodeError::NoImage(GraphicsAction::Delete))
        ));

        let short = kitty_payload(b"a=T,f=32,s=64,v=64", b"AAAA");
        assert!(matches!(short.decode(), Err(DecodeError::Malformed(_))));

        let sizeless = kitty_payload(b"a=T,f=32", b"AAAA");
        assert!(matches!(sizeless.decode(), Err(DecodeError::Malformed(_))));

        let mut builder = GraphicsBuilder::default();
        builder.kitty(b"a=T,f=32,s=2,v=1");
        builder.chunk(64, b"AAAABBBB", false, usize::MAX);
        let dropped = builder.finish().expect("a payload");
        assert_eq!(dropped.decode(), Err(DecodeError::NotCaptured));
    }

    #[cfg(feature = "decode")]
    #[test]
    fn a_sixel_decodes_into_the_pixels_it_paints() {
        // A 4x6 image: register 0 is white, and `~` sets all six rows of the
        // band, so a run of four fills the whole thing.
        let mut builder = GraphicsBuilder::default();
        builder.sixel();
        builder.chunk(40, b"\"1;1;4;6#0;2;100;100;100!4~-", true, usize::MAX);
        let bitmap = builder
            .finish()
            .expect("a payload")
            .decode()
            .expect("decodes");
        assert_eq!((bitmap.width(), bitmap.height()), (4, 6));
        for y in 0..6 {
            for x in 0..4 {
                assert_eq!(bitmap.pixel(x, y), Some([255, 255, 255, 255]), "({x},{y})");
            }
        }
    }

    #[cfg(feature = "decode")]
    #[test]
    fn a_sixel_pixel_nothing_painted_is_transparent_rather_than_black() {
        // `@` sets only the top row of the band; the five below it were
        // never written, and sixel has no alpha to say so with.
        let mut builder = GraphicsBuilder::default();
        builder.sixel();
        builder.chunk(30, b"\"1;1;1;6#0;2;0;100;0@-", true, usize::MAX);
        let bitmap = builder
            .finish()
            .expect("a payload")
            .decode()
            .expect("decodes");
        assert_eq!(bitmap.pixel(0, 0), Some([0, 255, 0, 255]));
        assert_eq!(bitmap.pixel(0, 1), Some([0, 0, 0, 0]));
    }

    #[cfg(feature = "decode")]
    #[test]
    fn sixel_bands_stack_downwards_and_carriage_returns_overprint() {
        // Two bands, and a `$` that goes back to paint the second colour
        // over the first band's second column.
        let mut builder = GraphicsBuilder::default();
        builder.sixel();
        builder.chunk(
            60,
            b"\"1;1;2;12#0;2;100;0;0~~$#1;2;0;0;100?~-#0??-",
            true,
            usize::MAX,
        );
        let bitmap = builder
            .finish()
            .expect("a payload")
            .decode()
            .expect("decodes");
        assert_eq!((bitmap.width(), bitmap.height()), (2, 12));
        assert_eq!(bitmap.pixel(0, 0), Some([255, 0, 0, 255]), "first colour");
        assert_eq!(bitmap.pixel(1, 0), Some([0, 0, 255, 255]), "overprinted");
        assert_eq!(bitmap.pixel(0, 6), Some([0, 0, 0, 0]), "second band");
    }

    #[cfg(feature = "decode")]
    #[test]
    fn a_sixel_without_raster_attributes_takes_its_size_from_its_data() {
        let mut builder = GraphicsBuilder::default();
        builder.sixel();
        builder.chunk(20, b"#0;2;100;100;100!3~-", true, usize::MAX);
        let bitmap = builder
            .finish()
            .expect("a payload")
            .decode()
            .expect("decodes");
        assert_eq!((bitmap.width(), bitmap.height()), (3, 6));
    }

    #[cfg(feature = "decode")]
    #[test]
    fn colours_are_counted_most_common_first() {
        let mut raw = vec![0u8; 0];
        for _ in 0..3 {
            raw.extend_from_slice(&[1, 2, 3, 255]);
        }
        raw.extend_from_slice(&[9, 9, 9, 255]);
        let data = base64(&raw);
        let payload = kitty_payload(b"a=T,f=32,s=4,v=1", data.as_bytes());
        let colours = payload.decode().expect("decodes").colours();
        assert_eq!(colours[0], ([1, 2, 3, 255], 3));
        assert_eq!(colours[1], ([9, 9, 9, 255], 1));
    }

    /// Equal counts come out in the order the colours first appear, so the
    /// answer is a property of the image rather than of a hash seed. Both
    /// orders, so a test that happened to agree by luck is ruled out.
    #[cfg(feature = "decode")]
    #[test]
    fn colours_break_ties_by_first_appearance() {
        let a = [1, 1, 1, 255];
        let b = [2, 2, 2, 255];
        let c = [3, 3, 3, 255];
        let image = |pixels: &[[u8; 4]]| {
            let raw: Vec<u8> = pixels.iter().flatten().copied().collect();
            let data = base64(&raw);
            kitty_payload(
                format!("a=T,f=32,s={},v=1", pixels.len()).as_bytes(),
                data.as_bytes(),
            )
            .decode()
            .expect("decodes")
            .colours()
        };
        assert_eq!(image(&[a, b, b, a, c]), vec![(a, 2), (b, 2), (c, 1)]);
        assert_eq!(image(&[b, a, a, b, c]), vec![(b, 2), (a, 2), (c, 1)]);
        // And the count still wins over appearance.
        assert_eq!(image(&[a, b, b]), vec![(b, 2), (a, 1)]);
    }

    /// A 512x512 image with every pixel a distinct colour — the shape of a
    /// photograph or a screenshot, and the one the issue extrapolated to
    /// seven seconds under the old per-pixel scan. The time is not asserted
    /// (a timing assertion is a flake waiting to happen); what is asserted
    /// is that the answer is complete and in raster order, which the
    /// quadratic version also got right, only slowly.
    #[cfg(feature = "decode")]
    #[test]
    fn colours_on_a_photograph_sized_image_completes() {
        let side = 512u32;
        let pixels = (side * side) as usize;
        let mut raw = Vec::with_capacity(pixels * 4);
        for i in 0..pixels {
            // i < 2^24, so the RGB triple is unique per pixel.
            raw.extend_from_slice(&[(i >> 16) as u8, (i >> 8) as u8, i as u8, 0xff]);
        }
        let data = base64(&raw);
        let bitmap = kitty_payload(
            format!("a=T,f=32,s={side},v={side}").as_bytes(),
            data.as_bytes(),
        )
        .decode()
        .expect("decodes");
        let colours = bitmap.colours();
        assert_eq!(colours.len(), pixels, "every pixel is its own colour");
        assert!(colours.iter().all(|&(_, count)| count == 1));
        // All tied at one, so the order is raster order.
        assert_eq!(colours[0].0, [0, 0, 0, 0xff]);
        assert_eq!(colours[1].0, [0, 0, 1, 0xff]);
        assert_eq!(colours[pixels - 1].0, [3, 0xff, 0xff, 0xff]);
    }

    #[cfg(feature = "decode")]
    #[test]
    fn hls_colours_are_refused_rather_than_converted() {
        let mut builder = GraphicsBuilder::default();
        builder.sixel();
        builder.chunk(30, b"\"1;1;1;6#0;1;120;50;100~-", true, usize::MAX);
        assert!(matches!(
            builder.finish().expect("a payload").decode(),
            Err(DecodeError::Unsupported(_))
        ));
    }

    #[test]
    fn the_debug_rendering_stays_short_enough_for_a_log() {
        let payload = kitty_payload(b"a=T,f=32,o=z,s=954,v=133,i=7,c=106,r=7", &[b'A'; 4096]);
        let rendered = format!("{payload:?}");
        assert!(rendered.len() < 120, "{rendered}");
        assert!(rendered.contains("954x133px"), "{rendered}");
        assert!(rendered.contains("106x7cells"), "{rendered}");
        assert!(!rendered.contains("AAAA"), "the data must not be in it");
    }

    /// The `Display` strings are the crate's contract with its callers, and the
    /// `f={value}` arm of `GraphicsFormat` had no coverage at all — a value
    /// other than 24/32/100 is the only input that reaches it.
    #[test]
    fn protocols_and_formats_render_as_their_names() {
        assert_eq!(GraphicsProtocol::Kitty.to_string(), "kitty");
        assert_eq!(GraphicsProtocol::Sixel.to_string(), "sixel");
        assert_eq!(GraphicsFormat::Rgb.to_string(), "rgb");
        assert_eq!(GraphicsFormat::Rgba.to_string(), "rgba");
        assert_eq!(GraphicsFormat::Png.to_string(), "png");
        assert_eq!(GraphicsFormat::Sixel.to_string(), "sixel");
        assert_eq!(GraphicsFormat::Other(99).to_string(), "f=99");
    }

    /// An unrecognized `f=` value must survive the parser as `Other`, never be
    /// guessed into a named format — driven through the real builder rather
    /// than hand-constructed, so a broken `f=` parse fails here.
    #[test]
    fn an_unknown_kitty_format_survives_named_and_rendered() {
        let payload = kitty_payload(b"a=T,f=99,s=1,v=1", b"AAAA");
        assert_eq!(payload.format(), GraphicsFormat::Other(99));
        assert_eq!(payload.format().to_string(), "f=99");
    }

    /// These are the strings a user actually reads when a decode is refused,
    /// so their exact wording is load-bearing (#181); the `assert_error`
    /// function pins the `std::error::Error` impl at compile time.
    #[cfg(feature = "decode")]
    #[test]
    fn every_decode_error_names_its_reason_and_is_an_error() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<DecodeError>();

        assert_eq!(
            DecodeError::NotCaptured.to_string(),
            "the payload was counted but not kept — raise TerminalBuilder::capture_graphics"
        );
        assert_eq!(
            DecodeError::NoImage(GraphicsAction::Delete).to_string(),
            "a Delete action carries no image data"
        );
        assert_eq!(
            DecodeError::Unsupported("kitty f=100 (PNG)").to_string(),
            "termlens does not decode kitty f=100 (PNG)"
        );
        assert_eq!(
            DecodeError::Malformed("bad base64").to_string(),
            "the payload carries bad base64"
        );
        assert_eq!(
            DecodeError::TooLarge("a sixel wider than 4096 pixels").to_string(),
            "termlens will not decode a sixel wider than 4096 pixels"
        );
    }

    /// The terse form is deliberate — a `Screen` is embedded in every error,
    /// and a derived `Debug` would dump the pixels into the log. This test is
    /// the fence against a `#[derive(Debug)]` being restored over the manual impl.
    #[cfg(feature = "decode")]
    #[test]
    fn a_bitmap_debugs_as_dimensions_only() {
        let raw = [0x40u8; 8];
        let bitmap = kitty_payload(b"a=T,f=32,s=2,v=1", base64(&raw).as_bytes())
            .decode()
            .expect("decodes");
        assert_eq!(format!("{bitmap:?}"), "Bitmap 2x1");
    }

    /// The encoder side of what `decode_base64` undoes — tests only.
    #[cfg(feature = "decode")]
    fn base64(data: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for group in data.chunks(3) {
            let mut bits = 0u32;
            for (index, byte) in group.iter().enumerate() {
                bits |= u32::from(*byte) << (16 - 8 * index);
            }
            for index in 0..=group.len() {
                out.push(ALPHABET[(bits >> (18 - 6 * index) & 0x3f) as usize] as char);
            }
            for _ in group.len()..3 {
                out.push('=');
            }
        }
        out
    }
}
