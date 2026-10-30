use serde::{
    Deserialize, Deserializer,
    de::{self, SeqAccess, Visitor},
};
use std::{fmt, marker::PhantomData};

pub(crate) fn bounded_vec<'de, D, T, const MAX: usize>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct V<T, const MAX: usize>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for V<T, MAX> {
        type Value = Vec<T>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            write!(f, "a sequence of at most {MAX} elements")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            // the declared length is untrusted: preallocate at most MAX
            let mut v = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX));
            while let Some(x) = seq.next_element()? {
                if v.len() == MAX {
                    return Err(de::Error::invalid_length(MAX + 1, &self));
                }
                v.push(x);
            }
            Ok(v)
        }
    }

    d.deserialize_seq(V::<T, MAX>(PhantomData))
}
