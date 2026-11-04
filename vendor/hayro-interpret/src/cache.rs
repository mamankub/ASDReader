use crate::util::hash128;
use hayro_syntax::object::{Array, Dict, MaybeRef, Name, Null, ObjRef, Object, Stream};
use kurbo::Affine;
use std::any::Any;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

type CacheMap = HashMap<u128, Option<Box<dyn Any + Send + Sync>>>;
/// The objects read from a document, by key. ASDReader patch (48): the second field counts the
/// ICC profiles read through it ([`Cache::icc_profile_reads`]).
#[derive(Clone)]
pub(crate) struct Cache(Arc<Mutex<CacheMap>>, Arc<AtomicUsize>);
impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self(Arc::new(Mutex::new(HashMap::new())), Arc::default())
    }

    /// ASDReader patch (48): count one ICC profile read, the colour space of an `/ICCBased`
    /// profile no entry held yet being built from it.
    pub(crate) fn count_icc_profile_read(&self) {
        self.1.fetch_add(1, Ordering::Relaxed);
    }

    /// ASDReader patch (48): how many ICC profiles were read through this cache.
    pub(crate) fn icc_profile_reads(&self) -> usize {
        self.1.load(Ordering::Relaxed)
    }

    pub(crate) fn get_or_insert_with<T: Clone + Send + Sync + 'static>(
        &self,
        id: u128,
        f: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        let mut locked = self.0.lock().unwrap();

        // We can't use `get_or_insert_with` here, because if the closure makes another access to the
        // cache, we end up with a deadlock.
        match locked.entry(id) {
            Entry::Occupied(o) => o
                .get()
                .as_ref()
                .and_then(|val| val.downcast_ref::<T>().cloned()),
            Entry::Vacant(_) => {
                drop(locked);
                let val = f();
                self.0.lock().unwrap().insert(
                    id,
                    val.clone()
                        .map(|val| Box::new(val) as Box<dyn Any + Send + Sync>),
                );

                val
            }
        }
    }
}

/// ASDReader patch: a byte-budgeted cache of decoded images, keyed by everything a decode reads
/// ([`DecodedImageKey`]). Image-heavy pages decode thousands of images; without this every tile
/// and every scale decodes them all again. Entries hold owned pixels, so a hit never changes
/// them; when an insert would pass the budget the whole map is dropped (a fresh epoch), which
/// only costs re-decoding, and an image larger than the whole budget is never kept. The budget
/// caps memory per interpreter cache. A poisoned lock decodes without the cache.
#[derive(Clone)]
pub(crate) struct DecodedImageCache {
    inner: Arc<Mutex<DecodedImageCacheInner>>,
    /// Whether the render this handle serves is a preview (`InterpreterSettings::preview`).
    /// Part of every key, so a preview's entries are never handed to a final render.
    pub(crate) preview: bool,
}

/// ASDReader patch: what a decoded image depends on. Two images share an entry only when all of
/// it is equal:
/// - where the stream's dictionary lies in the document's data (`dict_at`, `dict_len`): a
///   stream's data follows its dictionary, so this tells apart streams whose dictionaries are
///   byte for byte the same (two uncompressed images of one size), which `dict_hash` alone
///   would not; the data stays put for as long as the document (and so this cache) lives;
/// - the dictionary's bytes (`dict_hash`: `/Decode`, `/ColorSpace`, `/SMask`, `/Mask`,
///   `/BitsPerComponent`, filters, …) and its object (`obj`: the decryption key depends on it);
/// - whether it is decoded as a mask, a stencil mask, raster or mask output, interpolation,
///   the decode target size (only for an image a JPX stream decodes, which is the only
///   decode that reads it: ASDReader patch 32), and preview or final quality;
/// - the default colour space drawn in place of the image's device space, if any (the cache
///   key of its object, ASDReader patch 38): the same image can be drawn by resources with a
///   `/DefaultRGB` and by resources without one.
///
/// A transfer function (graphics state `/TR`) also changes the pixels and has no cheap key, so
/// an image drawn under one is not cached; neither is an inline image, whose dictionary lives in
/// a content stream and whose colour space can be a name resolved through the resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DecodedImageKey {
    pub(crate) mask_output: bool,
    pub(crate) dict_at: usize,
    pub(crate) dict_len: usize,
    pub(crate) dict_hash: u128,
    pub(crate) obj: Option<(i32, i32)>,
    pub(crate) is_mask: bool,
    pub(crate) is_stencil_mask: bool,
    pub(crate) interpolate: bool,
    pub(crate) default_space: Option<u128>,
    pub(crate) target: Option<(u32, u32)>,
    pub(crate) preview: bool,
}

/// One entry: filled once, by the first thread that asks for it; any other thread asking
/// meanwhile waits for that decode instead of running its own (ASDReader patch: several workers
/// drawing tiles of one page decoded its scanned underlay once each).
type Slot = Arc<std::sync::OnceLock<Option<Arc<dyn Any + Send + Sync>>>>;

struct DecodedImageCacheInner {
    map: HashMap<DecodedImageKey, Slot>,
    bytes: usize,
    /// ASDReader patch (32): how many keyed decodes ran (cache misses).
    decodes: usize,
    /// ASDReader patch: the most bytes of pixels kept.
    budget: usize,
}

std::thread_local! {
    /// ASDReader patch: the keys this thread is decoding, innermost last. A decode that needs a
    /// key it is already decoding (an image that is its own soft mask) decodes it again without
    /// the cache instead of waiting for itself.
    static DECODING: core::cell::RefCell<Vec<DecodedImageKey>> = const { core::cell::RefCell::new(Vec::new()) };
}

impl DecodedImageCache {
    pub(crate) fn new() -> Self {
        Self::with_budget(DEFAULT_IMAGE_BUDGET)
    }

    /// ASDReader patch: a cache that keeps at most `budget` bytes of pixels.
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DecodedImageCacheInner { map: HashMap::new(), bytes: 0, decodes: 0, budget })),
            preview: false,
        }
    }

    /// ASDReader patch (32): how many decodes the cache was asked to keep (its misses), for
    /// diagnostics and tests. 0 when the lock is poisoned.
    pub(crate) fn decodes(&self) -> usize {
        self.inner.lock().map(|locked| locked.decodes).unwrap_or(0)
    }

    /// The same cache, for a preview (`true`) or a final render.
    pub(crate) fn for_render(&self, preview: bool) -> Self {
        Self {
            inner: self.inner.clone(),
            preview,
        }
    }

    /// The decoded image for `key`, from the cache or from `f`. `None` as the key decodes
    /// without the cache. An image larger than the cache's budget is decoded each time.
    pub(crate) fn get_or_insert_with<T: Clone + Send + Sync + 'static>(
        &self,
        key: Option<DecodedImageKey>,
        size_of: impl FnOnce(&T) -> usize,
        f: impl FnOnce() -> Option<T>,
    ) -> Option<T> {
        let Some(key) = key else { return f() };
        if DECODING.with(|keys| keys.borrow().contains(&key)) {
            return f();
        }
        let slot = match self.inner.lock() {
            Ok(mut locked) => locked.map.entry(key).or_default().clone(),
            // A thread panicked while holding the lock: decode without the cache.
            Err(_) => return f(),
        };
        let mut decoded_here = None;
        let value = slot.get_or_init(|| {
            DECODING.with(|keys| keys.borrow_mut().push(key));
            let val = f();
            DECODING.with(|keys| keys.borrow_mut().pop());
            let size = val.as_ref().map_or(0, size_of);
            decoded_here = Some((size, val.clone()));
            val.map(|v| Arc::new(v) as Arc<dyn Any + Send + Sync>)
        });
        let hit = value.as_ref().and_then(|v| v.downcast_ref::<T>()).cloned();
        if let Some((size, val)) = decoded_here {
            if let Ok(mut locked) = self.inner.lock() {
                locked.decodes = locked.decodes.saturating_add(1);
                if size > locked.budget {
                    // Too large to keep: later renders decode it again.
                    locked.map.remove(&key);
                } else {
                    if locked.bytes.saturating_add(size) > locked.budget {
                        // A fresh epoch, keeping this one.
                        locked.map.retain(|k, _| *k == key);
                        locked.bytes = 0;
                    }
                    locked.bytes = locked.bytes.saturating_add(size);
                }
            }
            return val;
        }
        hit
    }
}

/// ASDReader patch: the pixel budget of a cache made by [`DecodedImageCache::new`].
const DEFAULT_IMAGE_BUDGET: usize = 64 << 20;

/// ASDReader patch: decoded images shared by the interpreter caches made with it
/// ([`InterpreterCache::with_images`](crate::InterpreterCache::with_images)), for several
/// workers rendering one document: an image is decoded once for all of them, and an image no
/// larger than `budget` bytes is kept for every later render.
#[derive(Clone)]
pub struct SharedImageCache(pub(crate) DecodedImageCache);

impl SharedImageCache {
    /// A cache that keeps at most `budget` bytes of decoded pixels.
    pub fn new(budget: usize) -> Self {
        Self(DecodedImageCache::with_budget(budget))
    }

    /// How many images were decoded through it (its misses), for diagnostics and tests.
    pub fn decodes(&self) -> usize {
        self.0.decodes()
    }
}

impl Default for SharedImageCache {
    fn default() -> Self {
        Self::new(DEFAULT_IMAGE_BUDGET)
    }
}

/// A trait for objects that can generate a unique cache key.
pub trait CacheKey {
    /// Returns the cache key for this object.
    fn cache_key(&self) -> u128;
}

impl<T: CacheKey, U: CacheKey> CacheKey for (T, U) {
    fn cache_key(&self) -> u128 {
        hash128(&(self.0.cache_key(), self.1.cache_key()))
    }
}

impl CacheKey for Dict<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self.data())
    }
}

impl CacheKey for Stream<'_> {
    fn cache_key(&self) -> u128 {
        self.dict().cache_key()
    }
}

impl CacheKey for Null {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl CacheKey for bool {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl CacheKey for hayro_syntax::object::Number {
    fn cache_key(&self) -> u128 {
        hash128(&self.as_f64().to_bits())
    }
}

impl CacheKey for hayro_syntax::object::String<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self.as_ref())
    }
}

impl CacheKey for Name<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl CacheKey for Array<'_> {
    fn cache_key(&self) -> u128 {
        hash128(self.data())
    }
}

impl CacheKey for Object<'_> {
    fn cache_key(&self) -> u128 {
        match self {
            Object::Null(n) => n.cache_key(),
            Object::Boolean(b) => b.cache_key(),
            Object::Number(n) => n.cache_key(),
            Object::String(s) => s.cache_key(),
            Object::Name(n) => n.cache_key(),
            Object::Dict(d) => d.cache_key(),
            Object::Array(a) => a.cache_key(),
            Object::Stream(s) => s.cache_key(),
        }
    }
}

impl CacheKey for ObjRef {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}

impl<T: CacheKey> CacheKey for MaybeRef<T> {
    fn cache_key(&self) -> u128 {
        match self {
            Self::Ref(r) => r.cache_key(),
            Self::NotRef(o) => o.cache_key(),
        }
    }
}

impl CacheKey for Affine {
    fn cache_key(&self) -> u128 {
        let c = self.as_coeffs();
        hash128(&[
            c[0].to_bits(),
            c[1].to_bits(),
            c[2].to_bits(),
            c[3].to_bits(),
            c[4].to_bits(),
            c[5].to_bits(),
        ])
    }
}

impl CacheKey for u128 {
    fn cache_key(&self) -> u128 {
        hash128(self)
    }
}
