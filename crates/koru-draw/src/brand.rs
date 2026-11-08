// SPDX-License-Identifier: MPL-2.0

//! Product marks: Koru's own logo, and the marks of the products Koru
//! launches.
//!
//! Two kinds live here. The **app's own mark** is a bitmap — decode it once
//! and draw it into whatever square a surface owns. Keeping the bitmap here
//! means the caption can become a menu anchor later without tying that menu
//! to the welcome view's implementation.
//!
//! The **launch products' marks** ([`BrandIcon`]) are third-party logos the
//! `+` menu's shell/agent rows and the session tabs wear (TERMINATAB Q-E6
//! P3/P4). They cannot be Phosphor glyphs — the icon font carries no other
//! product's logo — so they are the one place an icon is SVG path data
//! rather than a codepoint: parsed once with kurbo, filled in whatever
//! colour the surface passes, exactly the tinting contract a font glyph
//! gives. `scene::draw_brand_icon` is the single draw path.
//!
//! **vello has no mipmaps.** `Scene::draw_image` samples the source with one
//! bilinear tap (`ImageQuality::High` is a bicubic tap — still one), so a
//! 512 px master drawn into the caption's 20 px tile reads roughly one texel in
//! twenty-five and the spiral collapses into stairs. It is worst exactly where
//! it is most visible: the caption mark spins as the pane arrives, and which
//! texel each pixel lands on changes every frame, so the stairs crawl. So the
//! master is downscaled on the CPU, once per size, by a filter that reads
//! *every* source pixel — the mip chain vello does not keep.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::geometry::Rect;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Blob, ImageAlphaType, ImageBrush, ImageData, ImageFormat, ImageQuality};

/// The smallest mip worth keeping. Below this the mark is a dot, and the
/// caption tile is 20 px, so nothing asks for less in practice.
const MIN_MIP: u32 = 16;

/// The master, decoded once.
///
/// Kept as an `image` buffer rather than only as a brush because every mip is
/// resampled from it; `logo_brush` hands out the brush over the same pixels.
fn master() -> &'static image::RgbaImage {
    static MASTER: OnceLock<image::RgbaImage> = OnceLock::new();
    MASTER.get_or_init(|| {
        let mut rgba = image::load_from_memory(include_bytes!("../../../assets/logo.png"))
            .expect("embedded app logo is a valid image")
            .to_rgba8();
        // The channel's tint, applied once here so every mip, brush and
        // caller inherits it — a nightly window is amber in its own chrome,
        // not only in the taskbar. Stable is a no-op.
        koru_channel::tint::apply(&mut rgba, koru_channel::tint::tint());
        rgba
    })
}

fn brush_from(rgba: &image::RgbaImage) -> ImageBrush {
    let (width, height) = rgba.dimensions();
    ImageBrush::new(ImageData {
        data: Blob::new(Arc::new(rgba.as_raw().clone())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    })
    .with_quality(ImageQuality::High)
}

pub fn logo_brush() -> &'static ImageBrush {
    static LOGO: OnceLock<ImageBrush> = OnceLock::new();
    LOGO.get_or_init(|| brush_from(master()))
}

/// The master resampled to `size` square, cached.
///
/// Sizes are quantised to powers of two by the caller, so the cache holds a
/// handful of entries for the life of the process however the window is scaled
/// or zoomed — and an animation that squashes the mark cannot thrash it.
fn mip(size: u32) -> ImageBrush {
    static MIPS: OnceLock<Mutex<HashMap<u32, ImageBrush>>> = OnceLock::new();
    let mips = MIPS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut mips = mips.lock().expect("logo mip cache is not poisoned");
    mips.entry(size)
        .or_insert_with(|| {
            // Lanczos3 over the whole source: every one of the ~650 master
            // pixels behind a 20 px destination pixel contributes, which is
            // the difference between an antialiased edge and a staircase.
            let small = image::imageops::resize(
                master(),
                size,
                size,
                image::imageops::FilterType::Lanczos3,
            );
            brush_from(&small)
        })
        .clone()
}

/// The scale `transform` applies, in device pixels per logical pixel.
///
/// The larger axis wins so a mark that is squashed along one axis — the
/// caption's spin — still resamples from the detail the other axis needs, and
/// keeps asking for the same mip for the whole animation.
fn device_scale(transform: Affine) -> f64 {
    let [a, b, c, d, _, _] = transform.as_coeffs();
    a.hypot(b).max(c.hypot(d))
}

/// Which source size a destination of `wanted` device pixels should sample.
///
/// Rounded *up* to a power of two: never resample from less detail than the
/// destination needs, and never from wildly more. `None` asks for the master —
/// either because the destination is larger than it, or because there is
/// nothing sane to derive from a degenerate transform.
fn source_size(wanted: f64, native: u32) -> Option<u32> {
    if !wanted.is_finite() || wanted <= 0.0 || wanted >= native as f64 {
        return None;
    }
    Some((wanted as u32).next_power_of_two().clamp(MIN_MIP, native))
}

// ---------------------------------------------------------- launch products

/// A third-party product's mark, as monochrome SVG path data in a
/// [`VIEW_BOX`]-square view box.
///
/// The path is parsed once, on first draw; a malformed path is recorded as
/// `None` so it draws nothing rather than panicking a frame. The tests assert
/// every shipped mark parses, so `None` in practice means a future edit broke
/// the data — which then fails the test, not the app.
///
/// These are trademarks of their owners, used nominatively: each identifies
/// the product a profile row launches. Provenance and licence sit on each
/// constant.
pub struct BrandIcon {
    /// The product's name, for tests and debugging — never rendered.
    pub name: &'static str,
    /// SVG path data (the `d` attribute), single colour, [`VIEW_BOX`] square.
    pub path: &'static str,
    parsed: OnceLock<Option<vello::kurbo::BezPath>>,
}

/// The square every [`BrandIcon`] path is authored in, matching the icon
/// sets they were lifted from (Simple Icons and LobeHub both use 24).
pub const VIEW_BOX: f64 = 24.0;

impl BrandIcon {
    const fn new(name: &'static str, path: &'static str) -> Self {
        Self {
            name,
            path,
            parsed: OnceLock::new(),
        }
    }

    /// The parsed outline, or `None` for path data kurbo rejects.
    pub fn bez(&self) -> Option<&vello::kurbo::BezPath> {
        self.parsed
            .get_or_init(|| vello::kurbo::BezPath::from_svg(self.path).ok())
            .as_ref()
    }
}

/// Identity, not structure: every mark is a distinct `static`, and two
/// surfaces wear "the same icon" exactly when they point at the same one.
impl PartialEq for BrandIcon {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for BrandIcon {}

impl std::fmt::Debug for BrandIcon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrandIcon")
            .field("name", &self.name)
            .finish()
    }
}

/// Anthropic's Claude starburst.
/// Source: Simple Icons 16.29.0 `claude.svg` (icon data CC0-1.0; the mark is
/// Anthropic's).
pub static CLAUDE: BrandIcon = BrandIcon::new(
    "Claude",
    "m4.7144 15.9555 4.7174-2.6471.079-.2307-.079-.1275h-.2307l-.7893-.0486-2.6956-.0729-2.3375-.0971-2.2646-.1214-.5707-.1215-.5343-.7042.0546-.3522.4797-.3218.686.0608 1.5179.1032 2.2767.1578 1.6514.0972 2.4468.255h.3886l.0546-.1579-.1336-.0971-.1032-.0972L6.973 9.8356l-2.55-1.6879-1.3356-.9714-.7225-.4918-.3643-.4614-.1578-1.0078.6557-.7225.8803.0607.2246.0607.8925.686 1.9064 1.4754 2.4893 1.8336.3643.3035.1457-.1032.0182-.0728-.164-.2733-1.3539-2.4467-1.445-2.4893-.6435-1.032-.17-.6194c-.0607-.255-.1032-.4674-.1032-.7285L6.287.1335 6.6997 0l.9957.1336.419.3642.6192 1.4147 1.0018 2.2282 1.5543 3.0296.4553.8985.2429.8318.091.255h.1579v-.1457l.1275-1.706.2368-2.0947.2307-2.6957.0789-.7589.3764-.9107.7468-.4918.5828.2793.4797.686-.0668.4433-.2853 1.8517-.5586 2.9021-.3643 1.9429h.2125l.2429-.2429.9835-1.3053 1.6514-2.0643.7286-.8196.85-.9046.5464-.4311h1.0321l.759 1.1293-.34 1.1657-1.0625 1.3478-.8804 1.1414-1.2628 1.7-.7893 1.36.0729.1093.1882-.0183 2.8535-.607 1.5421-.2794 1.8396-.3157.8318.3886.091.3946-.3278.8075-1.967.4857-2.3072.4614-3.4364.8136-.0425.0304.0486.0607 1.5482.1457.6618.0364h1.621l3.0175.2247.7892.522.4736.6376-.079.4857-1.2142.6193-1.6393-.3886-3.825-.9107-1.3113-.3279h-.1822v.1093l1.0929 1.0686 2.0035 1.8092 2.5075 2.3314.1275.5768-.3218.4554-.34-.0486-2.2039-1.6575-.85-.7468-1.9246-1.621h-.1275v.17l.4432.6496 2.3436 3.5214.1214 1.0807-.17.3521-.6071.2125-.6679-.1214-1.3721-1.9246L14.38 17.959l-1.1414-1.9428-.1397.079-.674 7.2552-.3156.3703-.7286.2793-.6071-.4614-.3218-.7468.3218-1.4753.3886-1.9246.3157-1.53.2853-1.9004.17-.6314-.0121-.0425-.1397.0182-1.4328 1.9672-2.1796 2.9446-1.7243 1.8456-.4128.164-.7164-.3704.0667-.6618.4008-.5889 2.386-3.0357 1.4389-1.882.929-1.0868-.0062-.1579h-.0546l-6.3385 4.1164-1.1293.1457-.4857-.4554.0608-.7467.2307-.2429 1.9064-1.3114Z",
);

/// OpenCode's terminal-block mark (two subpaths; the inner one is wound the
/// other way, so a non-zero fill leaves it as the hole it draws in the
/// original).
/// Source: Simple Icons 16.29.0 `opencode.svg` (icon data CC0-1.0; the mark
/// is OpenCode's).
pub static OPENCODE: BrandIcon = BrandIcon::new("OpenCode", "M22 24H2V0h20zM17 4.8H7v14.4h10z");

/// OpenAI's knot, worn by **both** Codex profiles — Codex (Embedded) and
/// Codex CLI launch the same product, so they wear the same mark.
/// Source: Simple Icons 16.29.0 `openai.svg` (icon data CC0-1.0; the mark is
/// OpenAI's).
pub static OPENAI: BrandIcon = BrandIcon::new(
    "OpenAI",
    "M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997Z",
);

/// xAI's Grok swoosh. Not in Simple Icons (no `grok`/`xai` slug at 16.29.0),
/// so it comes from the other maintained monochrome AI-brand set.
/// Source: LobeHub `@lobehub/icons-static-svg` 1.95.0 `grok.svg` (icon
/// package MIT; the mark is xAI's).
pub static GROK: BrandIcon = BrandIcon::new(
    "Grok",
    "M9.27 15.29l7.978-5.897c.391-.29.95-.177 1.137.272.98 2.369.542 5.215-1.41 7.169-1.951 1.954-4.667 2.382-7.149 1.406l-2.711 1.257c3.889 2.661 8.611 2.003 11.562-.953 2.341-2.344 3.066-5.539 2.388-8.42l.006.007c-.983-4.232.242-5.924 2.75-9.383.06-.082.12-.164.179-.248l-3.301 3.305v-.01L9.267 15.292M7.623 16.723c-2.792-2.67-2.31-6.801.071-9.184 1.761-1.763 4.647-2.483 7.166-1.425l2.705-1.25a7.808 7.808 0 00-1.829-1A8.975 8.975 0 005.984 5.83c-2.533 2.536-3.33 6.436-1.962 9.764 1.022 2.487-.653 4.246-2.34 6.022-.599.63-1.199 1.259-1.682 1.925l7.62-6.815",
);

/// Moonshot's Kimi wordmark-K.
/// Source: Simple Icons 16.29.0 `kimi.svg` (icon data CC0-1.0; the mark is
/// Moonshot AI's).
pub static KIMI: BrandIcon = BrandIcon::new(
    "Kimi",
    "M21.765.351C22.998.351 24 1.353 24 2.586S22.998 4.82 21.765 4.82h-1.974c-.15 0-.26-.12-.26-.26V2.586A2.237 2.237 0 0 1 21.765.35M9.41 13.388l8.447-8.377c.16-.16.07-.471-.14-.471h-4.55s-.1.02-.14.06l-9.099 9.029c-.14.14-.35.02-.35-.21V4.81c0-.15-.1-.27-.221-.27H.22c-.12 0-.22.12-.22.27v18.57c0 .15.1.27.22.27h3.137c.12 0 .22-.12.22-.27v-3.79c0-.08.03-.16.08-.21l2.826-2.796c.07-.07.16-.08.241-.03l7.546 5.551a8.9 8.9 0 0 0 4.018 1.493c.12.01.23-.11.23-.27V19.76c0-.14-.08-.25-.19-.26a5.8 5.8 0 0 1-2.355-.942l-6.533-4.73c-.14-.09-.15-.32-.03-.441",
);

/// Microsoft's PowerShell chevron-in-a-window.
/// Source: Simple Icons 16.29.0 `powershell.svg` (icon data CC0-1.0; the mark
/// is Microsoft's).
pub static POWERSHELL: BrandIcon = BrandIcon::new(
    "PowerShell",
    "M23.181 2.974c.568 0 .923.463.792 1.035l-3.659 15.982c-.13.572-.697 1.035-1.265 1.035H.819c-.568 0-.923-.463-.792-1.035L3.686 4.009c.13-.572.697-1.035 1.265-1.035zm-8.375 9.346c.251-.394.227-.905-.09-1.243L9.122 5.125c-.38-.404-1.037-.407-1.466-.003-.429.402-.468 1.056-.088 1.46l4.662 4.96v.11l-7.42 5.374c-.45.327-.533.977-.187 1.453.346.476.991.597 1.44.27l8.229-5.91c.28-.196.438-.365.514-.52zm-2.796 4.399a.928.928 0 00-.934.923c0 .51.418.923.934.923h4.433a.928.928 0 00.934-.923.928.928 0 00-.934-.923z",
);

/// The GNU Bash hexagon.
/// Source: Simple Icons 16.29.0 `gnubash.svg` (icon data CC0-1.0; the mark is
/// the Free Software Foundation's).
pub static BASH: BrandIcon = BrandIcon::new(
    "Bash",
    "M21.038,4.9l-7.577-4.498C13.009,0.134,12.505,0,12,0c-0.505,0-1.009,0.134-1.462,0.403L2.961,4.9 C2.057,5.437,1.5,6.429,1.5,7.503v8.995c0,1.073,0.557,2.066,1.462,2.603l7.577,4.497C10.991,23.866,11.495,24,12,24 c0.505,0,1.009-0.134,1.461-0.402l7.577-4.497c0.904-0.537,1.462-1.529,1.462-2.603V7.503C22.5,6.429,21.943,5.437,21.038,4.9z M15.17,18.946l0.013,0.646c0.001,0.078-0.05,0.167-0.111,0.198l-0.383,0.22c-0.061,0.031-0.111-0.007-0.112-0.085L14.57,19.29 c-0.328,0.136-0.66,0.169-0.872,0.084c-0.04-0.016-0.057-0.075-0.041-0.142l0.139-0.584c0.011-0.046,0.036-0.092,0.069-0.121 c0.012-0.011,0.024-0.02,0.036-0.026c0.022-0.011,0.043-0.014,0.062-0.006c0.229,0.077,0.521,0.041,0.802-0.101 c0.357-0.181,0.596-0.545,0.592-0.907c-0.003-0.328-0.181-0.465-0.613-0.468c-0.55,0.001-1.064-0.107-1.072-0.917 c-0.007-0.667,0.34-1.361,0.889-1.8l-0.007-0.652c-0.001-0.08,0.048-0.168,0.111-0.2l0.37-0.236 c0.061-0.031,0.111,0.007,0.112,0.087l0.006,0.653c0.273-0.109,0.511-0.138,0.726-0.088c0.047,0.012,0.067,0.076,0.048,0.151 l-0.144,0.578c-0.011,0.044-0.036,0.088-0.065,0.116c-0.012,0.012-0.025,0.021-0.038,0.028c-0.019,0.01-0.038,0.013-0.057,0.009 c-0.098-0.022-0.332-0.073-0.699,0.113c-0.385,0.195-0.52,0.53-0.517,0.778c0.003,0.297,0.155,0.387,0.681,0.396 c0.7,0.012,1.003,0.318,1.01,1.023C16.105,17.747,15.736,18.491,15.17,18.946z M19.143,17.859c0,0.06-0.008,0.116-0.058,0.145 l-1.916,1.164c-0.05,0.029-0.09,0.004-0.09-0.056v-0.494c0-0.06,0.037-0.093,0.087-0.122l1.887-1.129 c0.05-0.029,0.09-0.004,0.09,0.056V17.859z M20.459,6.797l-7.168,4.427c-0.894,0.523-1.553,1.109-1.553,2.187v8.833 c0,0.645,0.26,1.063,0.66,1.184c-0.131,0.023-0.264,0.039-0.398,0.039c-0.42,0-0.833-0.114-1.197-0.33L3.226,18.64 c-0.741-0.44-1.201-1.261-1.201-2.142V7.503c0-0.881,0.46-1.702,1.201-2.142l7.577-4.498c0.363-0.216,0.777-0.33,1.197-0.33 c0.419,0,0.833,0.114,1.197,0.33l7.577,4.498c0.624,0.371,1.046,1.013,1.164,1.732C21.686,6.557,21.12,6.411,20.459,6.797z",
);

/// GitHub's Copilot face.
/// Source: Simple Icons `githubcopilot.svg` (icon data CC0-1.0; the mark is
/// GitHub's).
pub static COPILOT: BrandIcon = BrandIcon::new(
    "GitHub Copilot",
    "M23.922 16.997C23.061 18.492 18.063 22.02 12 22.02 5.937 22.02.939 18.492.078 16.997A.641.641 0 0 1 0 16.741v-2.869a.883.883 0 0 1 .053-.22c.372-.935 1.347-2.292 2.605-2.656.167-.429.414-1.055.644-1.517a10.098 10.098 0 0 1-.052-1.086c0-1.331.282-2.499 1.132-3.368.397-.406.89-.717 1.474-.952C7.255 2.937 9.248 1.98 11.978 1.98c2.731 0 4.767.957 6.166 2.093.584.235 1.077.546 1.474.952.85.869 1.132 2.037 1.132 3.368 0 .368-.014.733-.052 1.086.23.462.477 1.088.644 1.517 1.258.364 2.233 1.721 2.605 2.656a.841.841 0 0 1 .053.22v2.869a.641.641 0 0 1-.078.256Zm-11.75-5.992h-.344a4.359 4.359 0 0 1-.355.508c-.77.947-1.918 1.492-3.508 1.492-1.725 0-2.989-.359-3.782-1.259a2.137 2.137 0 0 1-.085-.104L4 11.746v6.585c1.435.779 4.514 2.179 8 2.179 3.486 0 6.565-1.4 8-2.179v-6.585l-.098-.104s-.033.045-.085.104c-.793.9-2.057 1.259-3.782 1.259-1.59 0-2.738-.545-3.508-1.492a4.359 4.359 0 0 1-.355-.508Zm2.328 3.25c.549 0 1 .451 1 1v2c0 .549-.451 1-1 1-.549 0-1-.451-1-1v-2c0-.549.451-1 1-1Zm-5 0c.549 0 1 .451 1 1v2c0 .549-.451 1-1 1-.549 0-1-.451-1-1v-2c0-.549.451-1 1-1Zm3.313-6.185c.136 1.057.403 1.913.878 2.497.442.544 1.134.938 2.344.938 1.573 0 2.292-.337 2.657-.751.384-.435.558-1.15.558-2.361 0-1.14-.243-1.847-.705-2.319-.477-.488-1.319-.862-2.824-1.025-1.487-.161-2.192.138-2.533.529-.269.307-.437.808-.438 1.578v.021c0 .265.021.562.063.893Zm-1.626 0c.042-.331.063-.628.063-.894v-.02c-.001-.77-.169-1.271-.438-1.578-.341-.391-1.046-.69-2.533-.529-1.505.163-2.347.537-2.824 1.025-.462.472-.705 1.179-.705 2.319 0 1.211.175 1.926.558 2.361.365.414 1.084.751 2.657.751 1.21 0 1.902-.394 2.344-.938.475-.584.742-1.44.878-2.497Z",
);

/// Google's Gemini four-pointed spark.
/// Source: Simple Icons `googlegemini.svg` (icon data CC0-1.0; the mark is
/// Google's).
pub static GEMINI: BrandIcon = BrandIcon::new(
    "Gemini",
    "M11.04 19.32Q12 21.51 12 24q0-2.49.93-4.68.96-2.19 2.58-3.81t3.81-2.55Q21.51 12 24 12q-2.49 0-4.68-.93a12.3 12.3 0 0 1-3.81-2.58 12.3 12.3 0 0 1-2.58-3.81Q12 2.49 12 0q0 2.49-.96 4.68-.93 2.19-2.55 3.81a12.3 12.3 0 0 1-3.81 2.58Q2.49 12 0 12q2.49 0 4.68.96 2.19.93 3.81 2.55t2.55 3.81",
);

/// Alibaba's Qwen mark, worn by the Qwen Code CLI.
/// Source: Simple Icons `qwen.svg` (icon data CC0-1.0; the mark is Alibaba's).
pub static QWEN: BrandIcon = BrandIcon::new(
    "Qwen",
    "M23.919 14.545 20.817 9.17l1.47-2.544a.56.56 0 0 0 0-.566l-1.633-2.83a.57.57 0 0 0-.49-.283h-6.207L12.487.402a.57.57 0 0 0-.49-.284H8.732a.56.56 0 0 0-.49.284L5.139 5.775h-2.94a.56.56 0 0 0-.49.284L.077 8.887a.56.56 0 0 0 0 .567L3.18 14.83l-1.47 2.545a.56.56 0 0 0 0 .566l1.634 2.83a.57.57 0 0 0 .49.283h6.205l1.47 2.545a.57.57 0 0 0 .49.284h3.266a.57.57 0 0 0 .49-.284l3.104-5.375h2.94a.57.57 0 0 0 .49-.283l1.634-2.828a.55.55 0 0 0-.004-.568M8.733.686l1.634 2.828-1.634 2.828H21.8L20.164 9.17H7.425L5.63 6.06Zm1.306 19.801-6.205-.002 1.634-2.83h3.265L2.201 6.344h3.267q3.182 5.517 6.367 11.032zm10.124-5.66L18.53 12l-6.532 11.315-1.634-2.83c2.129-3.673 4.25-7.351 6.373-11.028h3.592l3.102 5.374z",
);

/// Meta's infinity mark, worn by the Muse Code CLI — the company's mark, the
/// way Codex wears OpenAI's; there is no Muse-specific mark to source.
/// Source: Simple Icons 16.29.0 `meta.svg` (icon data CC0-1.0; the mark is
/// Meta's).
pub static META: BrandIcon = BrandIcon::new(
    "Meta",
    "M6.915 4.03c-1.968 0-3.683 1.28-4.871 3.113C.704 9.208 0 11.883 0 14.449c0 .706.07 1.369.21 1.973a6.624 6.624 0 0 0 .265.86 5.297 5.297 0 0 0 .371.761c.696 1.159 1.818 1.927 3.593 1.927 1.497 0 2.633-.671 3.965-2.444.76-1.012 1.144-1.626 2.663-4.32l.756-1.339.186-.325c.061.1.121.196.183.3l2.152 3.595c.724 1.21 1.665 2.556 2.47 3.314 1.046.987 1.992 1.22 3.06 1.22 1.075 0 1.876-.355 2.455-.843a3.743 3.743 0 0 0 .81-.973c.542-.939.861-2.127.861-3.745 0-2.72-.681-5.357-2.084-7.45-1.282-1.912-2.957-2.93-4.716-2.93-1.047 0-2.088.467-3.053 1.308-.652.57-1.257 1.29-1.82 2.05-.69-.875-1.335-1.547-1.958-2.056-1.182-.966-2.315-1.303-3.454-1.303zm10.16 2.053c1.147 0 2.188.758 2.992 1.999 1.132 1.748 1.647 4.195 1.647 6.4 0 1.548-.368 2.9-1.839 2.9-.58 0-1.027-.23-1.664-1.004-.496-.601-1.343-1.878-2.832-4.358l-.617-1.028a44.908 44.908 0 0 0-1.255-1.98c.07-.109.141-.224.211-.327 1.12-1.667 2.118-2.602 3.358-2.602zm-10.201.553c1.265 0 2.058.791 2.675 1.446.307.327.737.871 1.234 1.579l-1.02 1.566c-.757 1.163-1.882 3.017-2.837 4.338-1.191 1.649-1.81 1.817-2.486 1.817-.524 0-1.038-.237-1.383-.794-.263-.426-.464-1.13-.464-2.046 0-2.221.63-4.535 1.66-6.088.454-.687.964-1.226 1.533-1.533a2.264 2.264 0 0 1 1.088-.285z",
);

/// Every launch-product mark, for the parse test and a future picker.
pub static ALL_BRANDS: [&BrandIcon; 11] = [
    &CLAUDE,
    &OPENCODE,
    &OPENAI,
    &GROK,
    &KIMI,
    &POWERSHELL,
    &BASH,
    &COPILOT,
    &GEMINI,
    &QWEN,
    &META,
];

// ------------------------------------------------------ sign-in providers

/// Apple's logo, monochrome: filled in the label colour of the button it sits
/// on, so it greys out with a disabled one and turns light in dark mode.
/// Source: `C:\dev\design\Apple Logo.svg` (24 square, the artboard's
/// background rect dropped). The mark is Apple's.
pub static APPLE: BrandIcon = BrandIcon::new(
    "Apple",
    "M21.2807 18.424C20.9328 19.2275 20.5211 19.9672 20.0441 20.6472C19.3939 21.5743 18.8615 22.216 18.4512 22.5724C17.8152 23.1573 17.1337 23.4568 16.404 23.4739C15.8801 23.4739 15.2483 23.3248 14.5129 23.0224C13.7751 22.7214 13.097 22.5724 12.4771 22.5724C11.8268 22.5724 11.1295 22.7214 10.3836 23.0224C9.6365 23.3248 9.03469 23.4824 8.57456 23.498C7.87478 23.5278 7.17728 23.2197 6.48105 22.5724C6.03669 22.1848 5.48087 21.5204 4.81503 20.5791C4.10063 19.5739 3.51329 18.4084 3.05317 17.0795C2.56038 15.6442 2.31335 14.2543 2.31335 12.9087C2.31335 11.3673 2.64642 10.0379 3.31354 8.92385C3.83784 8.029 4.53534 7.32312 5.40832 6.80493C6.2813 6.28674 7.22456 6.02267 8.24036 6.00578C8.79618 6.00578 9.52506 6.1777 10.4308 6.51559C11.334 6.85462 11.914 7.02655 12.1683 7.02655C12.3584 7.02655 13.0026 6.82552 14.0948 6.42473C15.1277 6.05305 15.9994 5.89916 16.7135 5.95978C18.6485 6.11595 20.1023 6.87876 21.0691 8.25303C19.3385 9.30163 18.4824 10.7703 18.4995 12.6544C18.5151 14.122 19.0475 15.3432 20.0938 16.3129C20.568 16.7629 21.0975 17.1107 21.6867 17.3578C21.5589 17.7283 21.4241 18.0832 21.2807 18.424ZM16.8427 0.960131C16.8427 2.11039 16.4224 3.18439 15.5848 4.17847C14.574 5.36023 13.3513 6.04311 12.0254 5.93536C12.0086 5.79736 11.9988 5.65213 11.9988 5.49951C11.9988 4.39526 12.4795 3.21349 13.3331 2.24724C13.7593 1.75801 14.3014 1.35122 14.9587 1.02671C15.6146 0.707053 16.235 0.530273 16.8185 0.5C16.8356 0.653772 16.8427 0.807554 16.8427 0.960116V0.960131Z",
);

/// Google's `G`, as its four coloured parts. Each is one [`BrandIcon`] so the
/// same parse-once path draws it; the colours are Google's and do not tint.
/// Source: `C:\dev\design\Google Logo.svg` (24 square, the artboard's
/// background rect dropped). The mark is Google's.
pub static GOOGLE_BLUE: BrandIcon = BrandIcon::new(
    "Google (blue)",
    "M23.04 12.2614C23.04 11.4459 22.9668 10.6618 22.8309 9.90912H12V14.3575H18.1891C17.9225 15.795 17.1123 17.013 15.8943 17.8284V20.7139H19.6109C21.7855 18.7118 23.04 15.7637 23.04 12.2614Z",
);
pub static GOOGLE_GREEN: BrandIcon = BrandIcon::new(
    "Google (green)",
    "M12 23.4998C15.105 23.4998 17.7081 22.47 19.6109 20.7137L15.8943 17.8282C14.8645 18.5182 13.5472 18.9259 12 18.9259C9.00474 18.9259 6.46951 16.903 5.56519 14.1848H1.72314V17.1644C3.61542 20.9228 7.50451 23.4998 12 23.4998Z",
);
pub static GOOGLE_YELLOW: BrandIcon = BrandIcon::new(
    "Google (yellow)",
    "M5.56523 14.1851C5.33523 13.4951 5.20455 12.758 5.20455 12.0001C5.20455 11.2421 5.33523 10.5051 5.56523 9.81506V6.83551H1.72318C0.944318 8.38801 0.5 10.1444 0.5 12.0001C0.5 13.8557 0.944318 15.6121 1.72318 17.1646L5.56523 14.1851Z",
);
pub static GOOGLE_RED: BrandIcon = BrandIcon::new(
    "Google (red)",
    "M12 5.07386C13.6884 5.07386 15.2043 5.65409 16.3961 6.79364L19.6945 3.49523C17.7029 1.63955 15.0997 0.5 12 0.5C7.50451 0.5 3.61542 3.07705 1.72314 6.83545L5.56519 9.815C6.46951 7.09682 9.00474 5.07386 12 5.07386Z",
);

/// Every sign-in provider's mark, for the parse test.
pub static SIGN_IN_MARKS: [&BrandIcon; 5] = [
    &APPLE,
    &GOOGLE_BLUE,
    &GOOGLE_GREEN,
    &GOOGLE_YELLOW,
    &GOOGLE_RED,
];

/// What a launch-profile row or session tab wears: a Phosphor glyph, or a
/// vector brand mark for the products the icon font cannot carry.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ProfileMark {
    Glyph(char),
    Brand(&'static BrandIcon),
}

/// The mark for a built-in launch profile, by its **wire key** — the frozen
/// tokens of `koru_session_protocol::built_in_profile` (append-only per
/// ADR-263), matched here the way `icons::for_scheme` matches URL schemes so
/// the `+` menu and the tab strip cannot disagree about a profile's face.
///
/// `None` for a key this build does not know — a custom profile, or a
/// built-in this build predates — and the caller keeps its generic glyph.
/// The totality over today's catalogue is asserted where the catalogue is
/// visible (`apps/koru`'s context-menu tests): every `built_in_profiles()`
/// entry maps, and none to the generic terminal or sparkle.
pub fn for_profile(key: &str) -> Option<ProfileMark> {
    match key {
        // The *System* row: whatever the OS calls its own shell, so it wears
        // the OS's mark rather than a product's.
        "platform-shell" => Some(ProfileMark::Glyph(if cfg!(windows) {
            crate::icons::WINDOWS_LOGO
        } else if cfg!(target_os = "macos") {
            crate::icons::APPLE_LOGO
        } else {
            crate::icons::LINUX_LOGO
        })),
        "powershell" => Some(ProfileMark::Brand(&POWERSHELL)),
        "bash" => Some(ProfileMark::Brand(&BASH)),
        "claude" => Some(ProfileMark::Brand(&CLAUDE)),
        "opencode" => Some(ProfileMark::Brand(&OPENCODE)),
        // One product, two launch paths, one face.
        "codex" | "codex-cli" => Some(ProfileMark::Brand(&OPENAI)),
        "grok" => Some(ProfileMark::Brand(&GROK)),
        "kimi" => Some(ProfileMark::Brand(&KIMI)),
        "copilot" => Some(ProfileMark::Brand(&COPILOT)),
        "gemini" => Some(ProfileMark::Brand(&GEMINI)),
        "qwen" => Some(ProfileMark::Brand(&QWEN)),
        // Antigravity has no CC0 mark in the tree yet (Simple Icons carries
        // none); the robot glyph stands in until one is sourced (AGENTUX
        // QUESTIONS.md, phase D). Never Gemini's mark: a different product.
        "antigravity" => Some(ProfileMark::Glyph(crate::icons::ROBOT)),
        "muse" => Some(ProfileMark::Brand(&META)),
        // No licensed Devin or Cognition mark exists for the tree, and
        // Windsurf's is another product's: a neutral code glyph, distinct
        // from Antigravity's robot so the two rows are told apart.
        "devin" => Some(ProfileMark::Glyph(crate::icons::CODE)),
        _ => None,
    }
}

pub fn draw_logo(scene: &mut Scene, root: Affine, area: Rect) {
    if area.w <= 0.0 || area.h <= 0.0 {
        return;
    }
    let native = master().width().max(master().height());
    let wanted = (area.w.max(area.h) as f64 * device_scale(root)).ceil();
    let logo = match source_size(wanted, native) {
        Some(size) => mip(size),
        None => logo_brush().clone(),
    };

    let transform = root
        * Affine::translate((area.x as f64, area.y as f64))
        * Affine::scale_non_uniform(
            area.w as f64 / logo.image.width.max(1) as f64,
            area.h as f64 / logo.image.height.max(1) as f64,
        );
    scene.draw_image(&logo, transform);
}

/// `draw_logo` recoloured to a single colour: the bitmap's alpha channel is
/// the shape, the colour the fill — the same tinting contract a font glyph
/// gives, for surfaces the multicolour mark does not read on. Mips are cached
/// per (size, colour) exactly like the untinted chain.
pub fn draw_logo_tinted(scene: &mut Scene, root: Affine, area: Rect, color: [u8; 4]) {
    if area.w <= 0.0 || area.h <= 0.0 {
        return;
    }
    let native = master().width().max(master().height());
    let wanted = (area.w.max(area.h) as f64 * device_scale(root)).ceil();
    let size = source_size(wanted, native).unwrap_or(native);
    let logo = tinted_mip(size, color);

    let transform = root
        * Affine::translate((area.x as f64, area.y as f64))
        * Affine::scale_non_uniform(
            area.w as f64 / logo.image.width.max(1) as f64,
            area.h as f64 / logo.image.height.max(1) as f64,
        );
    scene.draw_image(&logo, transform);
}

/// The master resampled to `size` square and recoloured, cached per
/// (size, colour). The alpha channel is preserved (scaled by the colour's own
/// alpha); only the hue is replaced.
fn tinted_mip(size: u32, color: [u8; 4]) -> ImageBrush {
    static TINTED_MIPS: OnceLock<Mutex<HashMap<(u32, [u8; 4]), ImageBrush>>> = OnceLock::new();
    let mips = TINTED_MIPS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut mips = mips.lock().expect("tinted logo mip cache is not poisoned");
    mips.entry((size, color))
        .or_insert_with(|| {
            let mut small = image::imageops::resize(
                master(),
                size,
                size,
                image::imageops::FilterType::Lanczos3,
            );
            for pixel in small.pixels_mut() {
                pixel.0[0] = color[0];
                pixel.0[1] = color[1];
                pixel.0[2] = color[2];
                pixel.0[3] = (pixel.0[3] as u16 * color[3] as u16 / 255) as u8;
            }
            brush_from(&small)
        })
        .clone()
}

#[cfg(test)]
mod tests;
