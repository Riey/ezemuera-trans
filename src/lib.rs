use encoding_rs::UTF_16LE;
use eztrans_rs::EzTransLib;
use fxhash::FxHashMap;
use libloading::Library;
use serde_derive::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fs;
use std::path;
use std::ptr::null_mut;

pub struct EzDictItem {
    key: String,
    value: String,
}

impl EzDictItem {
    pub fn new(key: String, value: String) -> Self {
        Self { key, value }
    }

    pub fn apply(&self, text: &mut String) {
        while let Some(pos) = text.find(&self.key) {
            text.replace_range(pos..pos + self.key.len(), &self.key);
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

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
struct EzDict {
    #[serde(default)]
    sort: bool,
    #[serde(with = "dict_items")]
    #[serde(default)]
    before_dict: Vec<EzDictItem>,
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
    lib: EzTransLib,
    cache: FxHashMap<String, String>,
    dict: EzDict,
}

impl EzContext {
    pub fn from_path(
        lib: EzTransLib,
        path: &path::Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let cache_path = path.join("cache.yml");
        let dict_path = path.join("userdic.yml");
        let json_dict_path = path.join("userdic.json");

        let cache = if cache_path.exists() {
            serde_yaml::from_reader(fs::File::open(cache_path)?)?
        } else {
            FxHashMap::default()
        };

        let mut dict = if dict_path.exists() {
            serde_yaml::from_reader(fs::File::open(dict_path)?)?
        } else if json_dict_path.exists() {
            serde_json::from_reader(fs::File::open(json_dict_path)?)?
        } else {
            EzDict::default()
        };

        dict.sort();

        Ok(Self { lib, cache, dict })
    }

    pub fn translate(&mut self, text: &str) -> &str {
        let dict = &mut self.dict;
        let lib = &self.lib;

        self.cache.entry(text.into()).or_insert_with(move || {
            let mut original = text.into();

            for before in dict.before_dict.iter() {
                before.apply(&mut original);
            }

            let mut translated = match lib.translate(&original) {
                Ok(ret) => ret,
                Err(err) => {
                    eprintln!("translate err: {}", err);
                    return original;
                }
            };

            for after in dict.after_dict.iter() {
                after.apply(&mut translated);
            }

            translated
        })
    }
}

#[no_mangle]
pub unsafe extern "C" fn ez_init(ez_path: *const u16, ez_path_len: usize) -> *mut EzContext {
    let path = utf16_to_string(ez_path, ez_path_len);

    let lib = match Library::new(path.as_ref()) {
        Ok(lib) => lib,
        Err(err) => {
            eprintln!("Library loading failed: {:?}", err);
            return null_mut();
        }
    };

    let lib = match EzTransLib::new(lib) {
        Ok(lib) => lib,
        Err(err) => {
            eprintln!("Load EzTrans library failed: {:?}", err);
            return null_mut();
        }
    };

    let ctx = match EzContext::from_path(lib, path::Path::new(".")) {
        Ok(ctx) => ctx,
        Err(err) => {
            eprintln!("Loading context failed: {:?}", err);
            return null_mut();
        }
    };

    Box::into_raw(Box::new(ctx))
}

#[no_mangle]
pub unsafe extern "C" fn ez_delete(ptr: *mut EzContext) {
    let _ = Box::from_raw(ptr);
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

unsafe fn utf16_to_string<'a>(text: *const u16, len: usize) -> Cow<'a, str> {
    let (text, _) = UTF_16LE
        .decode_without_bom_handling(std::slice::from_raw_parts(text as *const u8, len * 2));

    text
}
