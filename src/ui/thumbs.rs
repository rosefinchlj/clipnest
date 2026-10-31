//! 缩略图缓存：后台线程解码 `blobs/thumbs/<id>.jpg`（回退原图 PNG），
//! UI 线程只做纹理上传与 LRU 管理，**绝不阻塞界面**。
//!
//! 规格（spec §12.1 / §15）：
//! - 最长边 96px、JPEG q80 存 `blobs/thumbs/xx/yy/<sha>.jpg`（xx/yy 为 sha 前 4 位）；
//! - 回退读取原图 `blobs/images/xx/yy/<sha>.png`；
//! - 内存 LRU 最多 64 张 / 16MB；解码失败静默占位，由调用方画占位图标；
//! - 1 条后台线程足够（缩略图数量受列表可见行限制）。

use crate::config;
use eframe::egui;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};


/// LRU 上限：张数。
pub const MAX_TEXTURES: usize = 64;
/// LRU 上限：字节数（RGBA 上传纹理的估算值）。
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
/// 缩略图最长边。
pub const THUMB_MAX_SIDE: u32 = 96;

// ---------------------------------------------------------------------------
// LRU 索引（纯逻辑，可单测）
// ---------------------------------------------------------------------------

/// 纹理 LRU 索引：记录顺序与字节数，返回需要淘汰的 id 列表。
#[derive(Debug, Default)]
pub struct LruIndex {
    order: VecDeque<String>,
    set: HashSet<String>,
    bytes: usize,
}

impl LruIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次访问（不存在则插入）。
    pub fn touch(&mut self, id: &str, bytes: usize) -> Vec<String> {
        let mut evicted = Vec::new();
        if self.set.contains(id) {
            // 移到队首（最新）
            if let Some(pos) = self.order.iter().position(|x| x == id) {
                self.order.remove(pos);
            }
            self.order.push_front(id.to_string());
            return evicted;
        }
        self.order.push_front(id.to_string());
        self.set.insert(id.to_string());
        self.bytes += bytes;
        while self.set.len() > MAX_TEXTURES
            || (self.bytes > MAX_BYTES && self.set.len() > 1)
        {
            let Some(old) = self.order.pop_back() else {
                break;
            };
            if self.set.remove(&old) {
                // 调用方负责给出被淘汰 id 对应的字节数；这里用返回值让调用方
                // 从 entries 里取真实大小后回减，避免两处状态不同步。
                evicted.push(old);
            }
        }
        evicted
    }

    /// 移除指定 id（`clear`/纹理释放时）。
    pub fn remove(&mut self, id: &str) {
        if self.set.remove(id) {
            if let Some(pos) = self.order.iter().position(|x| x == id) {
                self.order.remove(pos);
            }
        }
    }

    /// 淘汰后由调用方回减真实字节数。
    pub fn sub_bytes(&mut self, bytes: usize) {
        self.bytes = self.bytes.saturating_sub(bytes);
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.set.clear();
        self.bytes = 0;
    }
}

// ---------------------------------------------------------------------------
// 缩略图路径解析（纯函数，可单测）
// ---------------------------------------------------------------------------

/// 根据 blob id（sha256 十六进制）与数据目录生成候选路径（按优先级）。
///
/// 约定布局：`blobs/thumbs/<xx>/<yy>/<id>.jpg`、`blobs/images/<xx>/<yy>/<id>.png`。
pub fn thumb_candidates(blob_id: &str, data_dir: &std::path::Path) -> Vec<PathBuf> {
    let blobs = data_dir.join("blobs");
    let mut out = Vec::new();
    let clean: String = blob_id
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase();
    if clean.is_empty() {
        return out;
    }
    // 至少 4 位十六进制才视为有效 blob id（短 id 不做扁平回退，避免误命中）
    if clean.len() < 4 {
        return out;
    }
    {
        let xx = &clean[0..2];
        let yy = &clean[2..4];
        for ext in ["jpg", "jpeg", "png"] {
            out.push(blobs.join("thumbs").join(xx).join(yy).join(format!("{clean}.{ext}")));
        }
        for ext in ["png", "jpg"] {
            out.push(blobs.join("images").join(xx).join(yy).join(format!("{clean}.{ext}")));
        }
    }
    // 兼容扁平布局（早期数据/测试目录）
    for ext in ["jpg", "png"] {
        out.push(blobs.join("thumbs").join(format!("{clean}.{ext}")));
        out.push(blobs.join("images").join(format!("{clean}.{ext}")));
    }
    out
}

// ---------------------------------------------------------------------------
// 后台线程消息
// ---------------------------------------------------------------------------

struct ThumbRequest {
    id: String,
    candidates: Vec<PathBuf>,
}

struct ThumbResult {
    id: String,
    image: Option<egui::ColorImage>,
}

/// 后台线程：解码并按最长边 96 缩放（不 panic，失败发 None）。
fn worker_loop(rx: Receiver<ThumbRequest>, tx: Sender<ThumbResult>) {
    while let Ok(req) = rx.recv() {
        let image = decode_thumb(&req.candidates);
        // 接收端已关闭（缓存释放）→ 退出线程
        if tx.send(ThumbResult { id: req.id, image }).is_err() {
            break;
        }
    }
}

/// 依次尝试候选路径，返回缩放后的 RGBA 图像；全部失败返回 None。
fn decode_thumb(candidates: &[PathBuf]) -> Option<egui::ColorImage> {
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let Ok(img) = image::open(path) else {
            continue;
        };
        let thumb = img.thumbnail(THUMB_MAX_SIDE, THUMB_MAX_SIDE);
        let rgba = thumb.to_rgba8();
        let size = [rgba.width() as usize, rgba.height() as usize];
        return Some(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()));
    }
    None
}

// ---------------------------------------------------------------------------
// ThumbCache
// ---------------------------------------------------------------------------

struct Entry {
    texture: egui::TextureHandle,
    bytes: usize,
}

/// 缩略图缓存（线程安全边界：只在本 UI 线程使用）。
pub struct ThumbCache {
    ctx: egui::Context,
    tx: Option<Sender<ThumbRequest>>,
    rx: Option<Receiver<ThumbResult>>,
    entries: HashMap<String, Entry>,
    lru: LruIndex,
    /// 已发起请求但还没结果的 id。
    requested: HashSet<String>,
    /// 确认解码失败的 id（避免每帧重复请求）。
    failed: HashSet<String>,
}

impl ThumbCache {
    /// 创建缓存并启动 1 条后台解码线程；线程创建失败时退化为「无缩略图」模式。
    pub fn new(ctx: &egui::Context) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let (res_tx, res_rx) = std::sync::mpsc::channel();
        let spawn_ok = std::thread::Builder::new()
            .name("clipnest-thumb".to_string())
            .spawn(move || worker_loop(rx, res_tx))
            .is_ok();
        if !spawn_ok {
            crate::logging::warn("缩略图线程创建失败，列表将显示占位图标");
        }
        Self {
            ctx: ctx.clone(),
            tx: if spawn_ok { Some(tx) } else { None },
            rx: if spawn_ok { Some(res_rx) } else { None },
            entries: HashMap::new(),
            lru: LruIndex::new(),
            requested: HashSet::new(),
            failed: HashSet::new(),
        }
    }

    /// 取纹理：命中直接返回；未命中发后台请求并返回 None（本帧画占位）。
    pub fn get(&mut self, id: &str) -> Option<egui::TextureHandle> {
        self.pump();
        if let Some(entry) = self.entries.get(id) {
            let tex = entry.texture.clone();
            let bytes = entry.bytes;
            let evicted = self.lru.touch(id, bytes);
            self.release(evicted);
            return Some(tex);
        }
        if !self.failed.contains(id) && !self.requested.contains(id) {
            if let Some(tx) = &self.tx {
                let req = ThumbRequest {
                    id: id.to_string(),
                    candidates: thumb_candidates(id, &config::data_dir()),
                };
                if tx.send(req).is_ok() {
                    self.requested.insert(id.to_string());
                }
            }
        }
        None
    }

    /// 直接放入一张内存图像（预览假数据/测试用）。
    pub fn insert_image(&mut self, id: &str, image: egui::ColorImage) {
        self.store(id, image);
    }

    /// 清空缓存（设置变更/数据清空时由 Lead 调用）。
    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.requested.clear();
        self.failed.clear();
    }

    /// 记录一张图像的字节数（RGBA 估算）。
    fn store(&mut self, id: &str, image: egui::ColorImage) {
        let bytes = image.width() * image.height() * 4;
        let texture = self
            .ctx
            .load_texture(id.to_string(), image, egui::TextureOptions::LINEAR);
        self.entries.insert(
            id.to_string(),
            Entry {
                texture,
                bytes,
            },
        );
        // 新纹理计入 LRU；淘汰后释放资源
        let evicted = self.lru.touch(id, bytes);
        self.release(evicted);
    }

    /// 释放被淘汰的纹理（drop TextureHandle 即通知 egui 释放 GPU 纹理）。
    fn release(&mut self, ids: Vec<String>) {
        for id in ids {
            if let Some(entry) = self.entries.remove(&id) {
                self.lru.sub_bytes(entry.bytes);
            }
        }
    }

    /// 收取后台解码结果并上传纹理。
    fn pump(&mut self) {
        // 先收集结果再处理：避免持有 `&self.rx` 的同时可变借用 self。
        let mut results = Vec::new();
        if let Some(rx) = &self.rx {
            // 每帧最多处理 8 张，避免一次上传过多纹理造成卡顿
            for _ in 0..8 {
                match rx.try_recv() {
                    Ok(result) => results.push(result),
                    Err(_) => break,
                }
            }
        }
        for result in results {
            self.requested.remove(&result.id);
            match result.image {
                Some(image) => self.store(&result.id, image),
                None => {
                    self.failed.insert(result.id);
                }
            }
        }
    }

    /// 当前缓存张数（测试/诊断用）。
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 当前缓存字节数（测试/诊断用）。
    pub fn bytes(&self) -> usize {
        self.lru.bytes()
    }

    /// 是否已有可用纹理（不触发请求）。
    pub fn peek(&self, id: &str) -> bool {
        self.entries.contains_key(id)
    }
}

impl Drop for ThumbCache {
    fn drop(&mut self) {
        // 关闭发送端 → 后台线程 recv 返回 Err 后自然退出
        self.tx = None;
        self.rx = None;
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumb_paths_follow_layout() {
        let root = std::path::Path::new(r"C:\data");
        let cands = thumb_candidates("abcdef0123456789", root);
        let first = cands[0].to_string_lossy().replace('\\', "/");
        assert!(first.ends_with("blobs/thumbs/ab/cd/abcdef0123456789.jpg"));
        // 回退原图
        assert!(cands
            .iter()
            .any(|p| p.to_string_lossy().replace('\\', "/")
                .ends_with("blobs/images/ab/cd/abcdef0123456789.png")));
        // 非法 id 不 panic、返回空
        assert!(thumb_candidates("", root).is_empty());
        assert!(thumb_candidates("zz-not-hex", root).is_empty());
        // 短 id 走扁平回退
        let short = thumb_candidates("ab12", root);
        assert!(short
            .iter()
            .any(|p| p.to_string_lossy().replace('\\', "/").ends_with("blobs/thumbs/ab12.jpg")));
    }

    #[test]
    fn lru_evicts_over_count() {
        let mut lru = LruIndex::new();
        for i in 0..MAX_TEXTURES {
            assert!(lru.touch(&format!("id{i}"), 16).is_empty());
        }
        assert_eq!(lru.len(), MAX_TEXTURES);
        // 第 65 张 → 淘汰最旧的 id0
        let evicted = lru.touch("id-new", 16);
        assert_eq!(evicted, vec!["id0".to_string()]);
        assert_eq!(lru.len(), MAX_TEXTURES);
        // 命中已有 id 会刷新顺序，不会新增/淘汰
        assert!(lru.touch("id1", 16).is_empty());
    }

    #[test]
    fn lru_evicts_over_bytes_but_keeps_one() {
        let mut lru = LruIndex::new();
        let big = MAX_BYTES / 2 + 1;
        assert!(lru.touch("a", big).is_empty());
        // b 让总量超限 → 淘汰 a（调用方回减字节）
        let evicted = lru.touch("b", big);
        assert_eq!(evicted, vec!["a".to_string()]);
        lru.sub_bytes(big);
        assert_eq!(lru.len(), 1);
        // 再加一张大图仍超限 → 淘汰 b，但永远保留最新一张
        let evicted = lru.touch("c", big);
        assert_eq!(evicted, vec!["b".to_string()]);
        lru.sub_bytes(big);
        assert_eq!(lru.len(), 1);
        assert!(lru.bytes() <= MAX_BYTES);
    }

    #[test]
    fn failed_decode_returns_none() {
        assert!(decode_thumb(&[PathBuf::from(r"C:\no\such\file.jpg")]).is_none());
        assert!(decode_thumb(&[]).is_none());
    }
}
