use super::utils::{FloatOrInt, MergeWith};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Blur {
    pub off: bool,
    pub passes: u8,
    pub offset: f64,
    pub noise: f64,
    pub saturation: f64,
}

impl Default for Blur {
    fn default() -> Self {
        Self {
            off: false,
            passes: 3,
            offset: 3.,
            noise: 0.02,
            saturation: 1.5,
        }
    }
}

#[derive(knuffel::Decode, Debug, Default, Clone, Copy, PartialEq)]
pub struct BlurPart {
    #[knuffel(child)]
    pub off: bool,
    #[knuffel(child)]
    pub on: bool,
    #[knuffel(child, unwrap(argument))]
    pub passes: Option<u8>,
    #[knuffel(child, unwrap(argument))]
    pub offset: Option<FloatOrInt<0, 100>>,
    #[knuffel(child, unwrap(argument))]
    pub noise: Option<FloatOrInt<0, 1000>>,
    #[knuffel(child, unwrap(argument))]
    pub saturation: Option<FloatOrInt<0, 1000>>,
}

impl MergeWith<BlurPart> for Blur {
    fn merge_with(&mut self, part: &BlurPart) {
        self.off |= part.off;
        if part.on {
            self.off = false;
        }

        merge_clone!((self, part), passes);
        merge!((self, part), offset, noise, saturation);
    }
}
