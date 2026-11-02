//! DIB（与设备无关位图）解析 → RGBA，以及 PNG 编解码。
//!
//! 只做纯字节操作，不直接调用 Win32 API，因此可以完全用单元测试覆盖。
//!
//! 支持的变体（spec §10.3 / §12.1）：
//! - 位深：8（调色板）、16、24、32 bpp；
//! - 方向：bottom-up（biHeight > 0）与 top-down（biHeight < 0）；
//! - 压缩：BI_RGB、BI_BITFIELDS（含 BI_ALPHABITFIELDS 的 alpha 掩码）；
//! - V5/V4 头（`biSize` = 124/108）：掩码内嵌在头里；
//! - BI_RGB 的 32bpp 按约定忽略 alpha 字节（alpha 固定 255，避免黑底）；
//!   32bpp 的透明信息由回写时同时提供的注册格式 "PNG" 承载。
//!
//! 所有损坏/超限输入都返回 [`ClipError::Image`]，不 panic、不做未检查的切片。

use super::{ClipError, ClipResult};

/// 结构：解码后的 RGBA 位图。
#[derive(Debug)]
pub(super) struct DibImage {
    pub width: u32,
    pub height: u32,
    /// 长度恒为 `width * height * 4` 的 RGBA 数据（自上而下逐行）。
    pub rgba: Vec<u8>,
}

/// 解码硬上限：即使上层 limit 被配置得很大，也不做超过 1 亿像素的分配（防 OOM）。
const MAX_DECODE_PIXELS: u32 = 100_000_000;

// 压缩方式
const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

const BITMAPINFOHEADER_SIZE: usize = 40;

/// RGB 通道掩码。
#[derive(Clone, Copy, Debug)]
struct Masks {
    r: u32,
    g: u32,
    b: u32,
    /// 0 表示没有 alpha 通道（alpha 固定 255）。
    a: u32,
}

/// 解析出的 DIB 头信息。
#[derive(Debug)]
struct DibHeader {
    width: u32,
    height: u32,
    top_down: bool,
    bit_count: u16,
    masks: Option<Masks>,
    /// 8bpp 调色板起始偏移；非 8bpp 时无意义。
    palette_offset: usize,
    /// 调色板颜色数；非 8bpp 时为 0。
    palette_len: usize,
    /// 像素数据起始偏移。
    pixel_offset: usize,
}

// ---------------------------------------------------------------------------
// 对外（域内）API
// ---------------------------------------------------------------------------

/// DIB 字节 → RGBA。`max_pixels` 为像素数上限（宽 × 高），超出返回 Err。
pub(super) fn decode_dib_to_rgba(dib: &[u8], max_pixels: u32) -> ClipResult<DibImage> {
    let header = parse_header(dib)?;
    let pixels = header.width as u64 * header.height as u64;
    let effective_max = max_pixels.min(MAX_DECODE_PIXELS);
    if pixels > effective_max as u64 {
        return Err(ClipError::Image(format!(
            "图片 {}×{}（{} 像素）超过上限 {}",
            header.width, header.height, pixels, effective_max
        )));
    }

    let width = header.width as usize;
    let height = header.height as usize;
    // 每行按 4 字节对齐：stride = ceil(width * bpp / 32) * 4
    let stride = (width * header.bit_count as usize).div_ceil(32) * 4;
    let needed = stride
        .checked_mul(height)
        .ok_or_else(|| ClipError::Image("DIB 尺寸溢出".into()))?;
    let pixel_bytes = slice_at(dib, header.pixel_offset, needed)?;

    let mut rgba = vec![0u8; pixels as usize * 4];
    for src_y in 0..height {
        let row = &pixel_bytes[src_y * stride..(src_y + 1) * stride];
        let dst_y = if header.top_down {
            src_y
        } else {
            height - 1 - src_y
        };
        let dst = &mut rgba[dst_y * width * 4..(dst_y + 1) * width * 4];
        match header.bit_count {
            8 => decode_row_8(dib, &header, row, dst, width)?,
            16 => decode_row_16(&header, row, dst, width),
            24 => decode_row_24(row, dst, width),
            32 => decode_row_32(&header, row, dst, width),
            other => {
                return Err(ClipError::Image(format!("不支持的 DIB 位深 {other}")));
            }
        }
    }

    Ok(DibImage {
        width: header.width,
        height: header.height,
        rgba,
    })
}

/// PNG 字节 → RGBA（回写图片时用于生成 CF_DIB）。
pub(super) fn decode_png_to_rgba(png: &[u8], max_pixels: u32) -> ClipResult<(u32, u32, Vec<u8>)> {
    let (width, height) = png_dimensions(png)?;
    let pixels = width as u64 * height as u64;
    let effective_max = max_pixels.min(MAX_DECODE_PIXELS);
    if pixels > effective_max as u64 {
        return Err(ClipError::Image(format!(
            "PNG {}×{}（{} 像素）超过上限 {}",
            width, height, pixels, effective_max
        )));
    }
    let image = image::load_from_memory(png)
        .map_err(|err| ClipError::Image(format!("PNG 解码失败: {err}")))?;
    let rgba = image.to_rgba8();
    if rgba.width() != width || rgba.height() != height {
        return Err(ClipError::Image("PNG 头尺寸与解码结果不一致".into()));
    }
    Ok((width, height, rgba.into_raw()))
}

/// 仅校验 PNG 可解码并取尺寸（normalize 用；避免非法图片进入数据库）。
pub(super) fn validate_png(png: &[u8], max_pixels: u32) -> ClipResult<(u32, u32)> {
    let (width, height, rgba) = decode_png_to_rgba(png, max_pixels)?;
    drop(rgba); // 校验用，像素数据不入库（库中保存原始 PNG 字节）
    Ok((width, height))
}

/// 读取 PNG 头里的尺寸（不解码像素数据，代价很低）。
pub(super) fn png_dimensions(png: &[u8]) -> ClipResult<(u32, u32)> {
    let reader = image::ImageReader::new(std::io::Cursor::new(png))
        .with_guessed_format()
        .map_err(|err| ClipError::Image(format!("PNG 格式识别失败: {err}")))?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|err| ClipError::Image(format!("PNG 尺寸解析失败: {err}")))?;
    if width == 0 || height == 0 {
        return Err(ClipError::Image("PNG 尺寸为 0".into()));
    }
    Ok((width, height))
}

/// RGBA → PNG（压缩级别用 image 的 Default，对应 zlib 6 级，spec §12.1）。
pub(super) fn encode_png(rgba: &[u8], width: u32, height: u32) -> ClipResult<Vec<u8>> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{ExtendedColorType, ImageEncoder};

    if width == 0 || height == 0 {
        return Err(ClipError::Image("图片尺寸为 0".into()));
    }
    let expected = width as u64 * height as u64 * 4;
    if expected != rgba.len() as u64 {
        return Err(ClipError::Image(format!(
            "RGBA 缓冲区长度 {} 与尺寸 {}×{} 不匹配",
            rgba.len(),
            width,
            height
        )));
    }
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Default, FilterType::Adaptive)
        .write_image(rgba, width, height, ExtendedColorType::Rgba8)
        .map_err(|err| ClipError::Image(format!("PNG 编码失败: {err}")))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// DIB 头解析
// ---------------------------------------------------------------------------

fn parse_header(dib: &[u8]) -> ClipResult<DibHeader> {
    let header_size = u32_at(dib, 0)? as usize;
    if header_size < BITMAPINFOHEADER_SIZE {
        return Err(ClipError::Image(format!(
            "不支持的 DIB 头大小 {header_size}（需要 ≥40）"
        )));
    }
    // 保证头本身完整
    slice_at(dib, 0, header_size)?;

    let width_raw = i32_at(dib, 4)?;
    if width_raw <= 0 {
        return Err(ClipError::Image(format!("非法 DIB 宽度 {width_raw}")));
    }
    let height_raw = i32_at(dib, 8)?;
    if height_raw == 0 {
        return Err(ClipError::Image("非法 DIB 高度 0".into()));
    }
    let top_down = height_raw < 0;
    let width = width_raw as u32;
    let height = height_raw.unsigned_abs();

    let bit_count = u16_at(dib, 14)?;
    if !matches!(bit_count, 8 | 16 | 24 | 32) {
        return Err(ClipError::Image(format!("不支持的 DIB 位深 {bit_count}")));
    }
    let compression = u32_at(dib, 16)?;
    let is_bitfields = compression == BI_BITFIELDS || compression == BI_ALPHABITFIELDS;
    if compression != BI_RGB && !is_bitfields {
        return Err(ClipError::Image(format!(
            "不支持的 DIB 压缩类型 {compression}"
        )));
    }
    if is_bitfields && bit_count < 16 {
        return Err(ClipError::Image(format!(
            "{bit_count} 位 DIB 不支持 BI_BITFIELDS"
        )));
    }

    // 位域掩码：V4/V5 头内嵌在 40..56 字节；40 字节头则紧跟 3/4 个掩码
    let mut masks: Option<Masks> = None;
    let mut mask_bytes_after_header = 0usize;
    if is_bitfields {
        if header_size >= 56 {
            masks = Some(Masks {
                r: u32_at(dib, 40)?,
                g: u32_at(dib, 44)?,
                b: u32_at(dib, 48)?,
                a: u32_at(dib, 52)?,
            });
        } else if header_size == BITMAPINFOHEADER_SIZE {
            let mask_count = if compression == BI_ALPHABITFIELDS {
                4
            } else {
                3
            };
            mask_bytes_after_header = mask_count * 4;
            masks = Some(Masks {
                r: u32_at(dib, 40)?,
                g: u32_at(dib, 44)?,
                b: u32_at(dib, 48)?,
                a: if mask_count == 4 { u32_at(dib, 52)? } else { 0 },
            });
        } else {
            return Err(ClipError::Image(format!(
                "DIB 头大小 {header_size} 不支持位域掩码"
            )));
        }
    }
    // 16bpp BI_RGB 默认 RGB555
    let masks = masks.or({
        if bit_count == 16 {
            Some(Masks {
                r: 0x7C00,
                g: 0x03E0,
                b: 0x001F,
                a: 0,
            })
        } else {
            None
        }
    });

    let mut pixel_offset = header_size + mask_bytes_after_header;
    let mut palette_offset = 0usize;
    let mut palette_len = 0usize;
    if bit_count == 8 {
        let clr_used = u32_at(dib, 32)? as usize;
        let count = if clr_used == 0 {
            256
        } else {
            clr_used.min(256)
        };
        palette_offset = pixel_offset;
        palette_len = count;
        // 验证调色板完整
        slice_at(dib, palette_offset, count * 4)?;
        pixel_offset += count * 4;
    }

    Ok(DibHeader {
        width,
        height,
        top_down,
        bit_count,
        masks,
        palette_offset,
        palette_len,
        pixel_offset,
    })
}

// ---------------------------------------------------------------------------
// 逐行解码
// ---------------------------------------------------------------------------

fn decode_row_8(
    dib: &[u8],
    header: &DibHeader,
    row: &[u8],
    dst: &mut [u8],
    width: usize,
) -> ClipResult<()> {
    for x in 0..width {
        let index = row[x] as usize;
        if index >= header.palette_len {
            return Err(ClipError::Image(format!(
                "DIB 调色板索引 {index} 越界（共 {} 色）",
                header.palette_len
            )));
        }
        let off = header.palette_offset + index * 4;
        let b = dib[off];
        let g = dib[off + 1];
        let r = dib[off + 2];
        dst[x * 4] = r;
        dst[x * 4 + 1] = g;
        dst[x * 4 + 2] = b;
        dst[x * 4 + 3] = 255;
    }
    Ok(())
}

fn decode_row_16(header: &DibHeader, row: &[u8], dst: &mut [u8], width: usize) {
    let masks = header.masks.unwrap_or(Masks {
        r: 0x7C00,
        g: 0x03E0,
        b: 0x001F,
        a: 0,
    });
    for x in 0..width {
        let value = u16::from_le_bytes([row[x * 2], row[x * 2 + 1]]) as u32;
        write_rgba(dst, x, value, masks);
    }
}

fn decode_row_24(row: &[u8], dst: &mut [u8], width: usize) {
    for x in 0..width {
        let b = row[x * 3];
        let g = row[x * 3 + 1];
        let r = row[x * 3 + 2];
        dst[x * 4] = r;
        dst[x * 4 + 1] = g;
        dst[x * 4 + 2] = b;
        dst[x * 4 + 3] = 255;
    }
}

fn decode_row_32(header: &DibHeader, row: &[u8], dst: &mut [u8], width: usize) {
    for x in 0..width {
        let off = x * 4;
        let value = u32::from_le_bytes([row[off], row[off + 1], row[off + 2], row[off + 3]]);
        match header.masks {
            // BITFIELDS：按掩码取通道；没有 alpha 掩码时视为不透明
            Some(masks) => write_rgba(dst, x, value, masks),
            // BI_RGB 32bpp：内存序 B,G,R,A；按 spec 忽略 alpha
            None => {
                dst[x * 4] = row[off + 2];
                dst[x * 4 + 1] = row[off + 1];
                dst[x * 4 + 2] = row[off];
                dst[x * 4 + 3] = 255;
            }
        }
    }
}

/// 按掩码把 16/32bpp 像素值拆成 RGBA。
#[inline]
fn write_rgba(dst: &mut [u8], x: usize, value: u32, masks: Masks) {
    dst[x * 4] = extract_channel(value, masks.r);
    dst[x * 4 + 1] = extract_channel(value, masks.g);
    dst[x * 4 + 2] = extract_channel(value, masks.b);
    dst[x * 4 + 3] = if masks.a == 0 {
        255
    } else {
        extract_channel(value, masks.a)
    };
}

/// 从像素值中按掩码取一个通道并缩放到 0..=255（四舍五入）。
#[inline]
fn extract_channel(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = mask.count_ones();
    let raw = (value & mask) >> shift;
    // mask 最多 32 位，用 u64 计算缩放避免 (1 << 32) 溢出
    let max = (1u64 << bits) - 1;
    if max == 0 {
        return 0;
    }
    let scaled = (raw as u64 * 255 + max / 2) / max;
    scaled.min(255) as u8
}

// ---------------------------------------------------------------------------
// 安全读取小工具（全部返回 Err 而非 panic）
// ---------------------------------------------------------------------------

fn slice_at(data: &[u8], offset: usize, len: usize) -> ClipResult<&[u8]> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| ClipError::Image("DIB 偏移溢出".into()))?;
    data.get(offset..end)
        .ok_or_else(|| ClipError::Image("DIB 数据不完整".into()))
}

fn u16_at(data: &[u8], offset: usize) -> ClipResult<u16> {
    let b = slice_at(data, offset, 2)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(data: &[u8], offset: usize) -> ClipResult<u32> {
    let b = slice_at(data, offset, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn i32_at(data: &[u8], offset: usize) -> ClipResult<i32> {
    Ok(u32_at(data, offset)? as i32)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造 40 字节 BITMAPINFOHEADER。
    fn bitmapinfoheader(width: i32, height: i32, bit_count: u16, compression: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(40);
        out.extend_from_slice(&40u32.to_le_bytes()); // biSize
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        out.extend_from_slice(&bit_count.to_le_bytes());
        out.extend_from_slice(&compression.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes()); // biSizeImage
        out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
        out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
        out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
        assert_eq!(out.len(), 40);
        out
    }

    fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let off = ((y * width + x) * 4) as usize;
        [rgba[off], rgba[off + 1], rgba[off + 2], rgba[off + 3]]
    }

    #[test]
    fn decode_24bpp_bottom_up() {
        // 2×2，bottom-up：文件里第一行是图像的底行
        let mut dib = bitmapinfoheader(2, 2, 24, BI_RGB);
        // 底行（y=1）：左红右绿
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0]); // BGR
        dib.extend_from_slice(&[0, 0]); // 行对齐填充
                                        // 顶行（y=0）：左蓝右白
        dib.extend_from_slice(&[255, 0, 0, 255, 255, 255]);
        dib.extend_from_slice(&[0, 0]);

        let image = decode_dib_to_rgba(&dib, 100).expect("解码 24bpp");
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(pixel(&image.rgba, 2, 0, 0), [0, 0, 255, 255]); // 蓝
        assert_eq!(pixel(&image.rgba, 2, 1, 0), [255, 255, 255, 255]); // 白
        assert_eq!(pixel(&image.rgba, 2, 0, 1), [255, 0, 0, 255]); // 红
        assert_eq!(pixel(&image.rgba, 2, 1, 1), [0, 255, 0, 255]); // 绿
    }

    #[test]
    fn decode_32bpp_top_down_ignores_alpha_for_bi_rgb() {
        // 2×2，top-down（biHeight 为负）；BI_RGB 32bpp 忽略 alpha 字节
        let mut dib = bitmapinfoheader(2, -2, 32, BI_RGB);
        // 顶行：像素0 = (r=10,g=20,b=30,a=0)，像素1 = (r=40,g=50,b=60,a=128)
        dib.extend_from_slice(&[30, 20, 10, 0]);
        dib.extend_from_slice(&[60, 50, 40, 128]);
        // 底行
        dib.extend_from_slice(&[1, 2, 3, 7]);
        dib.extend_from_slice(&[4, 5, 6, 9]);

        let image = decode_dib_to_rgba(&dib, 100).expect("解码 32bpp");
        assert_eq!(pixel(&image.rgba, 2, 0, 0), [10, 20, 30, 255]);
        assert_eq!(pixel(&image.rgba, 2, 1, 0), [40, 50, 60, 255]);
        assert_eq!(pixel(&image.rgba, 2, 0, 1), [3, 2, 1, 255]);
        assert_eq!(pixel(&image.rgba, 2, 1, 1), [6, 5, 4, 255]);
    }

    #[test]
    fn decode_16bpp_bitfields_rgb565() {
        let mut dib = bitmapinfoheader(1, 1, 16, BI_BITFIELDS);
        // BI_BITFIELDS 掩码紧跟 40 字节头
        dib.extend_from_slice(&0xF800u32.to_le_bytes()); // R
        dib.extend_from_slice(&0x07E0u32.to_le_bytes()); // G
        dib.extend_from_slice(&0x001Fu32.to_le_bytes()); // B
                                                         // 1×1 16bpp stride = 4 字节（2 字节像素 + 2 字节填充）
        dib.extend_from_slice(&0xF800u16.to_le_bytes()); // 纯红
        dib.extend_from_slice(&0u16.to_le_bytes());

        let image = decode_dib_to_rgba(&dib, 100).expect("解码 16bpp BITFIELDS");
        assert_eq!(pixel(&image.rgba, 1, 0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn decode_16bpp_bitfields_555_green() {
        let mut dib = bitmapinfoheader(1, 1, 16, BI_BITFIELDS);
        dib.extend_from_slice(&0x7C00u32.to_le_bytes());
        dib.extend_from_slice(&0x03E0u32.to_le_bytes());
        dib.extend_from_slice(&0x001Fu32.to_le_bytes());
        dib.extend_from_slice(&(31u16 << 5).to_le_bytes()); // G=31
        dib.extend_from_slice(&0u16.to_le_bytes());

        let image = decode_dib_to_rgba(&dib, 100).expect("解码 16bpp 555");
        assert_eq!(pixel(&image.rgba, 1, 0, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn decode_v5_header_with_embedded_masks() {
        // BITMAPV5HEADER = 124 字节；掩码内嵌在偏移 40/44/48/52
        let mut dib = Vec::with_capacity(124 + 4);
        dib.extend_from_slice(&124u32.to_le_bytes()); // bV5Size
        dib.extend_from_slice(&1i32.to_le_bytes()); // width
        dib.extend_from_slice(&(-1i32).to_le_bytes()); // height（top-down）
        dib.extend_from_slice(&1u16.to_le_bytes()); // planes
        dib.extend_from_slice(&32u16.to_le_bytes()); // bit count
        dib.extend_from_slice(&BI_BITFIELDS.to_le_bytes());
        dib.extend_from_slice(&4u32.to_le_bytes()); // size image
        dib.extend_from_slice(&[0u8; 8]); // 两个 x/ypels
        dib.extend_from_slice(&0u32.to_le_bytes()); // clrUsed
        dib.extend_from_slice(&0u32.to_le_bytes()); // clrImportant
        dib.extend_from_slice(&0x00FF0000u32.to_le_bytes()); // RedMask
        dib.extend_from_slice(&0x0000FF00u32.to_le_bytes()); // GreenMask
        dib.extend_from_slice(&0x000000FFu32.to_le_bytes()); // BlueMask
        dib.extend_from_slice(&0xFF000000u32.to_le_bytes()); // AlphaMask
        dib.resize(124, 0); // 其余 V5 字段（色彩空间/端点等）填 0
        assert_eq!(dib.len(), 124);
        // 1×1 像素：0x11223344 → r=0x22, g=0x33, b=0x44, a=0x11
        dib.extend_from_slice(&0x11223344u32.to_le_bytes());

        let image = decode_dib_to_rgba(&dib, 100).expect("解码 V5 头");
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(pixel(&image.rgba, 1, 0, 0), [0x22, 0x33, 0x44, 0x11]);
    }

    #[test]
    fn decode_8bpp_palette() {
        let mut dib = bitmapinfoheader(4, 1, 8, BI_RGB);
        // 2 色调色板：0 = 蓝，1 = 红（RGBQUAD 字节序 B,G,R,0）
        dib.extend_from_slice(&[255, 0, 0, 0]);
        dib.extend_from_slice(&[0, 0, 255, 0]);
        // 修复 biClrUsed = 2（偏移 32）
        dib[32..36].copy_from_slice(&2u32.to_le_bytes());
        dib.extend_from_slice(&[0, 1, 0, 1]); // stride = 4 字节

        let image = decode_dib_to_rgba(&dib, 100).expect("解码 8bpp");
        assert_eq!(pixel(&image.rgba, 4, 0, 0), [0, 0, 255, 255]);
        assert_eq!(pixel(&image.rgba, 4, 1, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&image.rgba, 4, 2, 0), [0, 0, 255, 255]);
        assert_eq!(pixel(&image.rgba, 4, 3, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn decode_rejects_corrupt_input_without_panic() {
        let cases: Vec<Vec<u8>> = vec![
            Vec::new(),                                     // 空
            vec![0u8; 20],                                  // 头都不完整
            bitmapinfoheader(10, 10, 4, BI_RGB),            // 不支持位深
            bitmapinfoheader(10, 10, 32, 99),               // 不支持压缩
            bitmapinfoheader(0, 10, 32, BI_RGB),            // 宽为 0
            bitmapinfoheader(10, 0, 32, BI_RGB),            // 高为 0
            bitmapinfoheader(100_000, 100_000, 32, BI_RGB), // 超像素上限
            bitmapinfoheader(2, 2, 32, BI_RGB),             // 缺像素数据
        ];
        for (i, dib) in cases.iter().enumerate() {
            assert!(
                decode_dib_to_rgba(dib, 40_000_000).is_err(),
                "第 {i} 个损坏样本应返回错误"
            );
        }
        // biSize 小于 40 也应拒绝
        let mut short_header = bitmapinfoheader(1, 1, 32, BI_RGB);
        short_header[0..4].copy_from_slice(&12u32.to_le_bytes());
        assert!(decode_dib_to_rgba(&short_header, 100).is_err());
    }

    #[test]
    fn decode_rejects_truncated_pixel_data() {
        let mut dib = bitmapinfoheader(2, 2, 32, BI_RGB);
        dib.extend_from_slice(&[0u8; 8]); // 只给一行
        assert!(decode_dib_to_rgba(&dib, 100).is_err());
    }

    #[test]
    fn png_roundtrip_preserves_pixels() {
        let width = 3u32;
        let height = 2u32;
        let rgba: Vec<u8> = (0..(width * height * 4))
            .map(|i| (i * 7 % 256) as u8)
            .collect();
        let png = encode_png(&rgba, width, height).expect("PNG 编码");
        let (w, h, decoded) = decode_png_to_rgba(&png, 100).expect("PNG 解码");
        assert_eq!((w, h), (width, height));
        assert_eq!(decoded, rgba);
        assert_eq!(png_dimensions(&png).expect("PNG 尺寸"), (width, height));
        validate_png(&png, 100).expect("PNG 校验");
    }

    #[test]
    fn png_helpers_reject_bad_buffers() {
        assert!(encode_png(&[0, 0, 0], 1, 1).is_err()); // 长度不匹配
        assert!(encode_png(&[], 0, 1).is_err()); // 尺寸为 0
        assert!(decode_png_to_rgba(b"not png", 100).is_err());
        assert!(validate_png(&[], 100).is_err());
        assert!(png_dimensions(&[0u8; 3]).is_err());
        // 声明尺寸超限的 PNG：先用 1×1 合法 PNG 确认正常，再构造超限对象不可行，
        // 这里直接断言小上限能拒绝 3×2。
        let png = encode_png(&[0u8; 24], 3, 2).expect("编码");
        assert!(decode_png_to_rgba(&png, 5).is_err());
    }

    #[test]
    fn bitfield_alpha_scales_to_8bit() {
        // 4 位 alpha（0xF000）→ 满量程 255
        let masks = Masks {
            r: 0x0F00,
            g: 0x00F0,
            b: 0x000F,
            a: 0xF000,
        };
        let mut dst = [0u8; 4];
        write_rgba(&mut dst, 0, 0xF123, masks);
        assert_eq!(dst, [0x11, 0x22, 0x33, 0xFF]);
        // 没有 alpha 掩码 → 255
        let mut dst = [0u8; 4];
        write_rgba(&mut dst, 0, 0x0000, Masks { a: 0, ..masks });
        assert_eq!(dst[3], 255);
    }
}
