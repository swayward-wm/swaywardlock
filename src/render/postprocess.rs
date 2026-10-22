//! Background post-processing after the blur, on the GPU: hyprlock's vibrancy, then swayward's
//! blur saturation and noise (`shaders/postprocess.frag`). One draw from the blurred texture into
//! a new texture of the same size, once per background render (configure, scale change).
//!
//! A CPU pass (readback, per-pixel maths, upload) took 280-460 ms for a 3840x2160 background on
//! llvmpipe in a release build; this draw takes 60-80 ms there (vibrancy over 3 passes, the
//! heaviest case) and far less on a GPU.

use std::rc::Rc;

use anyhow::{ensure, Context as _};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{ffi, link_program, GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Offscreen as _, Texture as _};

/// What to apply. At the neutral values (vibrancy 0, saturation 1, noise 0) nothing runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PostProcess {
    /// Blur passes: hyprlock spreads the vibrancy over its down passes.
    pub passes: u8,
    pub vibrancy: f64,
    pub vibrancy_darkness: f64,
    pub saturation: f64,
    pub noise: f64,
}

impl PostProcess {
    pub fn from_config(bg: &crate::config::Background) -> Self {
        Self {
            passes: bg.blur.passes,
            vibrancy: bg.vibrancy,
            vibrancy_darkness: bg.vibrancy_darkness,
            saturation: bg.blur.saturation,
            noise: bg.blur.noise,
        }
    }

    pub fn is_neutral(&self) -> bool {
        self.vibrancy <= 0. && self.saturation == 1. && self.noise <= 0.
    }
}

#[derive(Debug, Clone)]
pub struct PostProcessProgram(Rc<Inner>);

#[derive(Debug)]
struct Inner {
    program: ffi::types::GLuint,
    uniform_tex: ffi::types::GLint,
    uniform_passes: ffi::types::GLint,
    uniform_vibrancy: ffi::types::GLint,
    uniform_vibrancy_darkness: ffi::types::GLint,
    uniform_saturation: ffi::types::GLint,
    uniform_noise: ffi::types::GLint,
    attrib_vert: ffi::types::GLint,
}

/// The compiled program (or `None` if it failed to compile), cached on the EGL context.
struct Cached(Option<PostProcessProgram>);

impl PostProcessProgram {
    /// The program for `renderer`'s context, compiled on first use. A compile error is logged
    /// once and gives `None` from then on.
    pub fn get(renderer: &mut GlesRenderer) -> Option<Self> {
        if let Some(Cached(p)) = renderer.egl_context().user_data().get::<Cached>() {
            return p.clone();
        }
        let p = Self::compile(renderer)
            .map_err(|err| tracing::warn!("background post-processing unavailable: {err:#}"))
            .ok();
        renderer
            .egl_context()
            .user_data()
            .insert_if_missing(|| Cached(p.clone()));
        p
    }

    pub fn compile(renderer: &mut GlesRenderer) -> anyhow::Result<Self> {
        renderer
            .with_context(|gl| unsafe {
                // The blur's full-screen vertex shader: v_coords runs 0..1 over the target.
                let program = link_program(
                    gl,
                    include_str!("shaders/blur.vert"),
                    include_str!("shaders/postprocess.frag"),
                )
                .context("error compiling the postprocess shader")?;
                let uniform = |name: &std::ffi::CStr| gl.GetUniformLocation(program, name.as_ptr());
                Ok(Self(Rc::new(Inner {
                    program,
                    uniform_tex: uniform(c"tex"),
                    uniform_passes: uniform(c"passes"),
                    uniform_vibrancy: uniform(c"vibrancy"),
                    uniform_vibrancy_darkness: uniform(c"vibrancy_darkness"),
                    uniform_saturation: uniform(c"saturation"),
                    uniform_noise: uniform(c"noise"),
                    attrib_vert: gl.GetAttribLocation(program, c"vert".as_ptr()),
                })))
            })
            .context("error making GL context current")?
    }

    /// Draw `source` post-processed into a new premultiplied RGBA texture of the same size.
    /// Orientation is kept: texel (x, y) of the source lands on texel (x, y) of the result.
    pub fn render(
        &self,
        renderer: &mut GlesRenderer,
        source: &GlesTexture,
        opts: &PostProcess,
    ) -> anyhow::Result<GlesTexture> {
        let size = source.size();
        let target: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, size)
            .context("error creating the post-processing texture")?;
        let p = &self.0;
        let (status, err) = renderer.with_context(|gl| unsafe {
            while gl.GetError() != ffi::NO_ERROR {}
            gl.Disable(ffi::BLEND);
            gl.Disable(ffi::SCISSOR_TEST);
            gl.ActiveTexture(ffi::TEXTURE0);

            let mut fbo = 0;
            gl.GenFramebuffers(1, &mut fbo);
            gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
            gl.FramebufferTexture2D(
                ffi::FRAMEBUFFER,
                ffi::COLOR_ATTACHMENT0,
                ffi::TEXTURE_2D,
                target.tex_id(),
                0,
            );
            let status = gl.CheckFramebufferStatus(ffi::FRAMEBUFFER);
            if status == ffi::FRAMEBUFFER_COMPLETE {
                gl.Viewport(0, 0, size.w, size.h);
                gl.UseProgram(p.program);
                gl.Uniform1i(p.uniform_tex, 0);
                gl.Uniform1i(p.uniform_passes, i32::from(opts.passes.clamp(1, 31)));
                gl.Uniform1f(p.uniform_vibrancy, opts.vibrancy as f32);
                gl.Uniform1f(p.uniform_vibrancy_darkness, opts.vibrancy_darkness as f32);
                gl.Uniform1f(p.uniform_saturation, opts.saturation as f32);
                gl.Uniform1f(p.uniform_noise, opts.noise as f32);

                gl.BindTexture(ffi::TEXTURE_2D, source.tex_id());
                // One texel per fragment: nearest keeps the colours exact.
                for (param, value) in [
                    (ffi::TEXTURE_MIN_FILTER, ffi::NEAREST),
                    (ffi::TEXTURE_MAG_FILTER, ffi::NEAREST),
                    (ffi::TEXTURE_WRAP_S, ffi::CLAMP_TO_EDGE),
                    (ffi::TEXTURE_WRAP_T, ffi::CLAMP_TO_EDGE),
                ] {
                    gl.TexParameteri(ffi::TEXTURE_2D, param, value as i32);
                }

                let vertices: [f32; 12] =
                    [0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
                gl.EnableVertexAttribArray(p.attrib_vert as u32);
                gl.BindBuffer(ffi::ARRAY_BUFFER, 0);
                gl.VertexAttribPointer(
                    p.attrib_vert as u32,
                    2,
                    ffi::FLOAT,
                    ffi::FALSE,
                    0,
                    vertices.as_ptr().cast(),
                );
                gl.DrawArrays(ffi::TRIANGLES, 0, 6);
                gl.DisableVertexAttribArray(p.attrib_vert as u32);
                gl.BindTexture(ffi::TEXTURE_2D, 0);
            }
            gl.BindFramebuffer(ffi::FRAMEBUFFER, 0);
            gl.DeleteFramebuffers(1, &fbo);
            (status, gl.GetError())
        })?;
        ensure!(
            status == ffi::FRAMEBUFFER_COMPLETE,
            "post-processing framebuffer incomplete ({status:#x})"
        );
        ensure!(
            err == ffi::NO_ERROR,
            "post-processing draw failed (GL error {err:#x})"
        );
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use smithay::backend::egl::native::EGLSurfacelessDisplay;
    use smithay::backend::egl::{EGLContext, EGLDisplay};
    use smithay::backend::renderer::{ExportMem as _, ImportMem as _};
    use smithay::utils::Rectangle;

    use super::*;

    /// A renderer on Mesa's surfaceless platform (llvmpipe without a GPU), or `None` when the
    /// machine has no EGL: the e2e tests cover the shader there.
    fn renderer() -> Option<GlesRenderer> {
        let made = (|| -> anyhow::Result<GlesRenderer> {
            let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay)? };
            let context = EGLContext::new(&display)?;
            Ok(unsafe { GlesRenderer::new(context)? })
        })();
        match made {
            Ok(r) => Some(r),
            Err(err) => {
                eprintln!("no surfaceless EGL ({err:#}): skipping the shader test");
                None
            }
        }
    }

    const NEUTRAL: PostProcess = PostProcess {
        passes: 1,
        vibrancy: 0.,
        vibrancy_darkness: 0.,
        saturation: 1.,
        noise: 0.,
    };

    /// Run the shader over a 4x4 texture of `rgba` (premultiplied) and return its RGBA pixels.
    fn run(
        r: &mut GlesRenderer,
        p: &PostProcessProgram,
        rgba: [u8; 4],
        opts: PostProcess,
    ) -> Vec<[u8; 4]> {
        let data = [rgba; 16].concat();
        let src = r
            .import_memory(&data, Fourcc::Abgr8888, (4, 4).into(), false)
            .unwrap();
        let out = p.render(r, &src, &opts).unwrap();
        let map = r
            .copy_texture(&out, Rectangle::from_size((4, 4).into()), Fourcc::Abgr8888)
            .unwrap();
        r.map_texture(&map)
            .unwrap()
            .chunks_exact(4)
            .map(|c| c.try_into().unwrap())
            .collect()
    }

    fn near(got: [u8; 4], want: [u8; 3], tol: i32) -> bool {
        (0..3).all(|i| (i32::from(got[i]) - i32::from(want[i])).abs() <= tol)
    }

    /// Reference values: a CPU port of hyprlock's FRAGBLUR1 vibrancy branch
    /// (scratch/vibrancy_ref.py) and swayward's postprocess.frag saturate.
    #[test]
    fn postprocess_follows_hyprlock_and_swayward() {
        let Some(mut r) = renderer() else { return };
        let p = PostProcessProgram::compile(&mut r).unwrap();
        let orange = [230, 120, 40, 255];

        // Vibrancy, hyprlock's FRAGBLUR1.
        let vib = |passes, vibrancy| PostProcess {
            passes,
            vibrancy,
            vibrancy_darkness: 0.05,
            ..NEUTRAL
        };
        for passes in [1, 3] {
            for px in run(&mut r, &p, orange, vib(passes, 1.)) {
                assert!(near(px, [255, 116, 15], 1), "passes {passes}: {px:?}");
                assert_eq!(px[3], 255, "alpha kept");
            }
        }
        let px = run(&mut r, &p, orange, vib(3, 0.1686))[5];
        assert!(near(px, [244, 118, 26], 1), "hyprlock's default: {px:?}");
        assert_eq!(run(&mut r, &p, orange, vib(1, 0.))[5], orange, "vibrancy 0");
        let grey = [128, 128, 128, 255];
        assert_eq!(
            run(&mut r, &p, grey, vib(1, 1.))[5],
            grey,
            "nothing to boost"
        );
        let px = run(&mut r, &p, [200, 60, 60, 255], vib(1, 1.))[5];
        assert!(near(px, [201, 59, 59], 1), "{px:?}");

        // Saturation, swayward's postprocess.frag.
        let sat = |saturation| PostProcess {
            saturation,
            ..NEUTRAL
        };
        let px = run(&mut r, &p, orange, sat(0.5))[0];
        assert!(near(px, [184, 129, 89], 1), "{px:?}");
        let px = run(&mut r, &p, orange, sat(0.))[0];
        assert!(near(px, [138, 138, 138], 1), "luma: {px:?}");
        let px = run(&mut r, &p, orange, sat(2.))[0];
        assert!(near(px, [255, 102, 0], 1), "clamped: {px:?}");
        assert_eq!(run(&mut r, &p, orange, NEUTRAL)[0], orange);

        // Noise, swayward's postprocess.frag: grey, at most a quarter of the range at 0.5, and
        // different from pixel to pixel.
        let out = run(
            &mut r,
            &p,
            grey,
            PostProcess {
                noise: 0.5,
                ..NEUTRAL
            },
        );
        for px in &out {
            assert!(near(*px, [128, 128, 128], 64), "{px:?}");
            assert!(px[0] == px[1] && px[1] == px[2] && px[3] == 255, "{px:?}");
        }
        let distinct: std::collections::HashSet<_> = out.iter().collect();
        assert!(distinct.len() > 1, "noise varies with the pixel: {out:?}");

        // Premultiplied input: the maths runs on the colour, alpha is kept.
        let half = [115, 60, 20, 128];
        assert_eq!(run(&mut r, &p, half, sat(0.99))[0][3], 128);
        let px = run(&mut r, &p, half, sat(0.))[0];
        assert_eq!(px[3], 128);
        // luma 137.6 at full alpha, so about 69 at alpha 128.
        assert!(near(px, [69, 69, 69], 1), "{px:?}");
        let all = PostProcess {
            vibrancy: 1.,
            saturation: 2.,
            noise: 0.5,
            ..NEUTRAL
        };
        assert_eq!(
            run(&mut r, &p, [0; 4], all)[0],
            [0; 4],
            "transparent stays transparent"
        );
    }

    /// The result is not flipped or mirrored.
    #[test]
    fn postprocess_keeps_orientation() {
        let Some(mut r) = renderer() else { return };
        let p = PostProcessProgram::compile(&mut r).unwrap();
        // 2x2: red, green / blue, white.
        let data = [
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [255, 255, 255, 255],
        ]
        .concat();
        let src = r
            .import_memory(&data, Fourcc::Abgr8888, (2, 2).into(), false)
            .unwrap();
        let opts = PostProcess {
            saturation: 1.0001,
            ..NEUTRAL
        };
        let out = p.render(&mut r, &src, &opts).unwrap();
        let map = r
            .copy_texture(&out, Rectangle::from_size((2, 2).into()), Fourcc::Abgr8888)
            .unwrap();
        assert_eq!(r.map_texture(&map).unwrap(), &data[..]);
    }
}
