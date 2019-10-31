use encoding_rs::{EncoderResult, EUC_KR, SHIFT_JIS, UTF_16LE};
use eztrans_rs::{Container, EzTransLib};
use fxhash::FxHashMap;
use serde_derive::{Deserialize, Serialize};
use std::borrow::Cow;
use std::ffi::CStr;
use std::fs;
use std::path::Path;
use std::ptr::null_mut;

pub struct EzDictItem {
    key: String,
    value: String,
}

impl EzDictItem {
    pub fn new(key: String, value: String) -> Self {
        assert!(!key.is_empty());
        Self { key, value }
    }

    pub fn apply(&self, text: &mut String) {
        let mut prev_pos = 0;
        while let Some(pos) = twoway::find_str(&text[prev_pos..], &self.key) {
            text.replace_range(pos..pos + self.key.len(), &self.value);
            prev_pos = pos + self.value.len();
        }
    }

    #[inline]
    pub fn key(&self) -> &str {
        &self.key
    }

    #[inline]
    pub fn value(&self) -> &str {
        &self.value
    }
}

#[test]
fn dict_item_test() {
    let item = EzDictItem::new("123".into(), "abc".into());
    let mut foo = "123def".into();
    item.apply(&mut foo);
    assert_eq!(foo, "abcdef");
}

#[test]
#[should_panic]
fn dict_item_empty_key_test() {
    let _item = EzDictItem::new("".into(), "123".into());
}

#[test]
fn dict_item_empty_value_test() {
    let item = EzDictItem::new("123".into(), "".into());
    let mut foo = "123def".into();
    item.apply(&mut foo);
    assert_eq!(foo, "def");
}

#[test]
fn dict_item_eq_kv_test() {
    let item = EzDictItem::new("123".into(), "123".into());
    let mut foo = "123def".into();
    item.apply(&mut foo);
    assert_eq!(foo, "123def");
}

#[derive(Serialize, Deserialize, Default)]
struct EzDict {
    #[serde(default)]
    sort: bool,
    #[serde(alias = "BeforeDic")]
    #[serde(with = "dict_items")]
    #[serde(default)]
    before_dict: Vec<EzDictItem>,
    #[serde(alias = "AfterDic")]
    #[serde(with = "dict_items")]
    #[serde(default)]
    after_dict: Vec<EzDictItem>,
}

impl EzDict {
    pub fn sort_before_dict(&mut self) {
        if !self.sort {
            return;
        }

        self.before_dict
            .sort_unstable_by(|l, r| l.key().cmp(r.key()));
    }

    pub fn sort_after_dict(&mut self) {
        if !self.sort {
            return;
        }

        self.after_dict
            .sort_unstable_by(|l, r| l.key().cmp(r.key()));
    }

    pub fn sort(&mut self) {
        self.sort_after_dict();
        self.sort_before_dict();
    }
}

mod dict_items {
    use super::EzDictItem;
    use serde::de::{MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S: Serializer>(items: &Vec<EzDictItem>, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(items.len()))?;

        for item in items {
            map.serialize_entry(item.key(), item.value())?;
        }

        map.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<EzDictItem>, D::Error> {
        struct ItemVisitor;

        impl<'de> Visitor<'de> for ItemVisitor {
            type Value = Vec<EzDictItem>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("key and value")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut access: M) -> Result<Self::Value, M::Error> {
                let mut ret = Vec::with_capacity(access.size_hint().unwrap_or(10));

                while let Some((key, value)) = access.next_entry()? {
                    ret.push(EzDictItem::new(key, value));
                }

                Ok(ret)
            }
        }

        d.deserialize_map(ItemVisitor)
    }
}

pub struct EzContext {
    lib: Container<EzTransLib<'static>>,
    cache: FxHashMap<String, String>,
    dict: EzDict,
    encode_buffer: Vec<u8>,
    string_buffer: String,
}

impl EzContext {
    pub fn from_path(
        lib: Container<EzTransLib<'static>>,
        path: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let cache_path = path.join("cache.msgpack");
        let dict_path = path.join("userdic.yml");
        let json_dict_path = path.join("userdic.json");

        let mut cache = if cache_path.exists() {
            rmp_serde::from_read(fs::File::open(cache_path)?)?
        } else {
            FxHashMap::default()
        };

        cache.insert(String::new(), String::new());

        let mut dict = if dict_path.exists() {
            serde_yaml::from_reader(fs::File::open(dict_path)?)?
        } else if json_dict_path.exists() {
            serde_json::from_reader(fs::File::open(json_dict_path)?)?
        } else {
            EzDict::default()
        };

        dict.sort();

        Ok(Self {
            lib,
            cache,
            dict,
            encode_buffer: Vec::with_capacity(8192),
            string_buffer: String::new(),
        })
    }

    pub fn save_to(&self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let cache_path = path.join("cache.msgpack");
        let dict_path = path.join("userdic.yml");

        use std::fs::write;

        write(cache_path, rmp_serde::to_vec(&self.cache)?)?;
        write(dict_path, serde_yaml::to_vec(&self.dict)?)?;

        Ok(())
    }

    fn translate_impl(&mut self, text: &str) -> &str {
        let dict = &mut self.dict;
        let lib = &self.lib;
        let buf = &mut self.encode_buffer;
        let str_buf = &mut self.string_buffer;

        self.cache.entry(text.into()).or_insert_with(move || {
            str_buf.push_str(text);

            let mut encoder = SHIFT_JIS.new_encoder();
            let mut decoder = EUC_KR.new_decoder_without_bom_handling();

            let max_buf_len = encoder
                .max_buffer_length_from_utf8_without_replacement(str_buf.len())
                .unwrap_or(0);

            buf.reserve(max_buf_len + 1);

            let (encoder_ret, _) =
                encoder.encode_from_utf8_to_vec_without_replacement(&str_buf, buf, true);

            buf.push(0);

            assert_eq!(encoder_ret, EncoderResult::InputEmpty);

            let translated =
                unsafe { lib.translate(CStr::from_bytes_with_nul_unchecked(&buf[..])) };
            let translated = translated.as_bytes();

            buf.clear();
            str_buf.clear();

            let mut ret = String::with_capacity(
                decoder
                    .max_utf8_buffer_length_without_replacement(translated.len())
                    .unwrap_or(0),
            );
            let (_decoder_ret, _) =
                decoder.decode_to_string_without_replacement(translated, &mut ret, true);

            for after in dict.after_dict.iter() {
                after.apply(&mut ret);
            }

            ret
        })
    }

    pub fn translate(&mut self, text: &str) -> &str {
        if !self.cache.contains_key(text) {
            let max_len = UTF_16LE
                .new_decoder_without_bom_handling()
                .max_utf8_buffer_length_without_replacement(text.len() * 2);
            let mut ret = String::with_capacity(max_len.unwrap_or(text.len() * 3));

            {
                let mut text = text.into();

                for before in self.dict.before_dict.iter() {
                    before.apply(&mut text);
                }

                let mut prev_pos = 0;
                let mut is_in_japanese = is_japanese(text.chars().next().unwrap());

                for (pos, ch) in text.char_indices() {
                    if is_japanese(ch) {
                        if !is_in_japanese {
                            ret.push_str(&text[prev_pos..=pos]);

                            prev_pos = pos;
                            is_in_japanese = true;
                        }
                    } else {
                        if is_in_japanese {
                            let translated = self.translate_impl(&text[prev_pos..=pos]);
                            ret.push_str(translated);

                            prev_pos = pos;
                            is_in_japanese = false;
                        }
                    }
                }

                if !is_in_japanese {
                    ret.push_str(&text[prev_pos..]);
                } else {
                    let translated = self.translate_impl(&text[prev_pos..]);
                    ret.push_str(translated);
                }
            }

            self.cache.insert(text.into(), ret);
        }

        self.cache.get(text).unwrap()
    }
}

#[no_mangle]
pub unsafe extern "C" fn ez_init(
    ez_path: *const u16,
    ez_path_len: usize,
    ctx_path: *const u16,
    ctx_path_len: usize,
) -> *mut EzContext {
    let path = utf16_to_string(ez_path, ez_path_len);
    let ctx_path = utf16_to_string(ctx_path, ctx_path_len);
    let path = Path::new(path.as_ref());
    let ctx_path = Path::new(ctx_path.as_ref());

    eprintln!("Loading lib from {}", path.display());

    let lib = match eztrans_rs::load_library(path.join("J2KEngine.dll")) {
        Ok(lib) => lib,
        Err(err) => {
            eprintln!("EzTrans library loading failed: {:?}", err);
            return null_mut();
        }
    };

    let mut dat_dir = path.join("Dat").to_str().unwrap().to_string().into_bytes();
    dat_dir.push(0);

    lib.initialize(
        CStr::from_bytes_with_nul_unchecked(b"CSUSER123455\0"),
        CStr::from_bytes_with_nul_unchecked(&dat_dir[..]),
    );

    let ctx = match EzContext::from_path(lib, ctx_path) {
        Ok(ctx) => ctx,
        Err(err) => {
            eprintln!("Loading context failed: {:?}", err);
            return null_mut();
        }
    };

    Box::into_raw(Box::new(ctx))
}

#[no_mangle]
pub unsafe extern "C" fn ez_save(ctx: *mut EzContext, path: *const u16, path_len: usize) {
    let path = utf16_to_string(path, path_len);
    let path = Path::new(path.as_ref());

    if let Err(err) = (*ctx).save_to(path) {
        eprintln!("Save err: {:?}", err);
    }
}

#[no_mangle]
pub unsafe extern "C" fn ez_delete(ctx: *mut EzContext) {
    let _ = Box::from_raw(ctx);
}

#[no_mangle]
pub unsafe extern "C" fn ez_add_before_dict(
    ctx: *mut EzContext,
    key: *const u16,
    key_len: usize,
    value: *const u16,
    value_len: usize,
) {
    let key = utf16_to_string(key, key_len);
    let value = utf16_to_string(value, value_len);

    (*ctx)
        .dict
        .before_dict
        .push(EzDictItem::new(key.into_owned(), value.into_owned()));
    (*ctx).dict.sort_before_dict();
}

#[no_mangle]
pub unsafe extern "C" fn ez_add_after_dict(
    ctx: *mut EzContext,
    key: *const u16,
    key_len: usize,
    value: *const u16,
    value_len: usize,
) {
    let key = utf16_to_string(key, key_len);
    let value = utf16_to_string(value, value_len);

    (*ctx)
        .dict
        .after_dict
        .push(EzDictItem::new(key.into_owned(), value.into_owned()));
    (*ctx).dict.sort_after_dict();
}

#[no_mangle]
pub unsafe extern "C" fn ez_translate(
    ctx: *mut EzContext,
    text: *const u16,
    text_len: usize,
    out_text: *mut *const u8,
    out_text_len: *mut usize,
) -> i32 {
    let text = utf16_to_string(text, text_len);

    let translated = (*ctx).translate(text.as_ref());

    *out_text = translated.as_ptr();
    *out_text_len = translated.len();

    0
}

fn u16_slice_to_u8_slice(slice: &[u16]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(slice.as_ptr() as *const u8, slice.len() * 2) }
}

unsafe fn utf16_to_string<'a>(text: *const u16, len: usize) -> Cow<'a, str> {
    let (text, _) = UTF_16LE
        .decode_without_bom_handling(u16_slice_to_u8_slice(std::slice::from_raw_parts(text, len)));

    text
}

fn is_japanese(ch: char) -> bool {
    let ch = ch as u32;
    (ch >= 0x3000 && ch <= 0x30FF) || (ch >= 0x4E00 && ch <= 0x9FAF)
}
