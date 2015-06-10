/*!

Handles binding uniforms to the OpenGL state machine.

*/
use gl;
use sync;

use std::cell::RefCell;
use std::collections::HashMap;

use BufferViewExt;
use BufferViewSliceExt;
use DrawError;
use ProgramExt;
use UniformsExt;
use RawUniformValue;

use uniforms::Uniforms;
use uniforms::UniformValue;
use uniforms::SamplerBehavior;

use context::CommandContext;
use ContextExt;
use QueryExt;

use utils::bitsfield::Bitsfield;

use sampler_object::SamplerObject;
use GlObject;
use vertex::MultiVerticesSource;

use program;
use context;
use version::Version;
use version::Api;

impl<U> UniformsExt for U where U: Uniforms {
    fn bind_uniforms<'a, P>(&'a self, mut ctxt: &mut CommandContext, program: &P,
                        fences: &mut Vec<&'a RefCell<Option<sync::LinearSyncFence>>>,
                        samplers: &mut HashMap<SamplerBehavior, SamplerObject>) -> Result<(), DrawError>
                        where P: ProgramExt
    {
        let mut texture_bind_points = Bitsfield::new();
        let mut uniform_buffer_bind_points = Bitsfield::new();
        let mut shared_storage_buffer_bind_points = Bitsfield::new();

        let mut visiting_result = Ok(());
        self.visit_values(|name, value| {
            if visiting_result.is_err() { return; }

            if let Some(uniform) = program.get_uniform(name) {
                assert!(uniform.size.is_none(), "Uniform arrays not supported yet");

                if !value.is_usable_with(&uniform.ty) {
                    visiting_result = Err(DrawError::UniformTypeMismatch {
                        name: name.to_string(),
                        expected: uniform.ty,
                    });
                    return;
                }

                match bind_uniform(&mut ctxt, samplers, &value, program, uniform.location,
                                   &mut texture_bind_points, name)
                {
                    Ok(_) => (),
                    Err(e) => {
                        visiting_result = Err(e);
                        return;
                    }
                };

            } else if let Some(block) = program.get_uniform_blocks().get(name) {
                let fence = match bind_uniform_block(&mut ctxt, &value, block,
                                                     program, &mut uniform_buffer_bind_points, name)
                {
                    Ok(f) => f,
                    Err(e) => {
                        visiting_result = Err(e);
                        return;
                    }
                };

                if let Some(fence) = fence {
                    fences.push(fence);
                }

            } else if let Some(block) = program.get_shader_storage_blocks().get(name) {
                let fence = match bind_shared_storage_block(&mut ctxt, &value, block, program,
                                                            &mut shared_storage_buffer_bind_points,
                                                            name)
                {
                    Ok(f) => f,
                    Err(e) => {
                        visiting_result = Err(e);
                        return;
                    }
                };

                if let Some(fence) = fence {
                    fences.push(fence);
                }
            }
        });

        visiting_result
    }
}

fn bind_uniform_block<'a, P>(ctxt: &mut context::CommandContext, value: &UniformValue<'a>,
                             block: &program::UniformBlock,
                             program: &P, buffer_bind_points: &mut Bitsfield, name: &str)
                             -> Result<Option<&'a RefCell<Option<sync::LinearSyncFence>>>, DrawError>
                             where P: ProgramExt
{
    match value {
        &UniformValue::Block(buffer, ref layout) => {
            if !layout(block) {
                return Err(DrawError::UniformBlockLayoutMismatch { name: name.to_string() });
            }

            let bind_point = buffer_bind_points.get_unused().expect("Not enough buffer units");
            buffer_bind_points.set_used(bind_point);

            assert!(buffer.get_offset_bytes() == 0);     // TODO: not implemented
            let fence = buffer.add_fence();
            let binding = block.binding as gl::types::GLuint;

            buffer.prepare_and_bind_for_uniform(ctxt, bind_point as gl::types::GLuint);
            program.set_uniform_block_binding(ctxt, binding, bind_point as gl::types::GLuint);

            Ok(fence)
        },
        _ => {
            Err(DrawError::UniformValueToBlock { name: name.to_string() })
        }
    }
}

fn bind_shared_storage_block<'a, P>(ctxt: &mut context::CommandContext, value: &UniformValue<'a>,
                                    block: &program::UniformBlock,
                                    program: &P, buffer_bind_points: &mut Bitsfield, name: &str)
                                    -> Result<Option<&'a RefCell<Option<sync::LinearSyncFence>>>, DrawError>
                                    where P: ProgramExt
{
    match value {
        &UniformValue::Block(buffer, ref layout) => {
            if !layout(block) {
                return Err(DrawError::UniformBlockLayoutMismatch { name: name.to_string() });
            }

            let bind_point = buffer_bind_points.get_unused().expect("Not enough buffer units");
            buffer_bind_points.set_used(bind_point);

            assert!(buffer.get_offset_bytes() == 0);     // TODO: not implemented
            let fence = buffer.add_fence();
            let binding = block.binding as gl::types::GLuint;

            buffer.prepare_and_bind_for_shared_storage(ctxt, bind_point as gl::types::GLuint);
            program.set_shader_storage_block_binding(ctxt, binding, bind_point as gl::types::GLuint);

            Ok(fence)
        },
        _ => {
            Err(DrawError::UniformValueToBlock { name: name.to_string() })
        }
    }
}

fn bind_uniform<P>(ctxt: &mut context::CommandContext,
                   samplers: &mut HashMap<SamplerBehavior, SamplerObject>,
                   value: &UniformValue, program: &P, location: gl::types::GLint,
                   texture_bind_points: &mut Bitsfield, name: &str)
                   -> Result<(), DrawError> where P: ProgramExt
{
    assert!(location >= 0);

    match *value {
        UniformValue::Block(_, _) => {
            Err(DrawError::UniformBufferToValue {
                name: name.to_string(),
            })
        },
        UniformValue::SignedInt(val) => {
            program.set_uniform(ctxt, location, &RawUniformValue::SignedInt(val));
            Ok(())
        },
        UniformValue::UnsignedInt(val) => {
            program.set_uniform(ctxt, location, &RawUniformValue::UnsignedInt(val));
            Ok(())
        },
        UniformValue::Float(val) => {
            program.set_uniform(ctxt, location, &RawUniformValue::Float(val));
            Ok(())
        },
        UniformValue::Mat2(val) => {
            program.set_uniform(ctxt, location, &RawUniformValue::Mat2(val));
            Ok(())
        },
        UniformValue::Mat3(val) => {
            program.set_uniform(ctxt, location, &RawUniformValue::Mat3(val));
            Ok(())
        },
        UniformValue::Mat4(val) => {
            program.set_uniform(ctxt, location, &RawUniformValue::Mat4(val));
            Ok(())
        },
        UniformValue::Vec2(val) => {
            program.set_uniform(ctxt, location, &RawUniformValue::Vec2(val));
            Ok(())
        },
        UniformValue::Vec3(val) => {
            program.set_uniform(ctxt, location, &RawUniformValue::Vec3(val));
            Ok(())
        },
        UniformValue::Vec4(val) => {
            program.set_uniform(ctxt, location, &RawUniformValue::Vec4(val));
            Ok(())
        },
        UniformValue::Texture1d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D)
        },
        UniformValue::CompressedTexture1d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D)
        },
        UniformValue::SrgbTexture1d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D)
        },
        UniformValue::CompressedSrgbTexture1d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D)
        },
        UniformValue::IntegralTexture1d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D)
        },
        UniformValue::UnsignedTexture1d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D)
        },
        UniformValue::DepthTexture1d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D)
        },
        UniformValue::Texture2d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D)
        },
        UniformValue::CompressedTexture2d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D)
        },
        UniformValue::SrgbTexture2d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D)
        },
        UniformValue::CompressedSrgbTexture2d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D)
        },
        UniformValue::IntegralTexture2d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D)
        },
        UniformValue::UnsignedTexture2d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D)
        },
        UniformValue::DepthTexture2d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D)
        },
        UniformValue::Texture2dMultisample(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE)
        },
        UniformValue::SrgbTexture2dMultisample(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE)
        },
        UniformValue::IntegralTexture2dMultisample(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE)
        },
        UniformValue::UnsignedTexture2dMultisample(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE)
        },
        UniformValue::DepthTexture2dMultisample(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE)
        },
        UniformValue::Texture3d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_3D)
        },
        UniformValue::CompressedTexture3d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_3D)
        },
        UniformValue::SrgbTexture3d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_3D)
        },
        UniformValue::CompressedSrgbTexture3d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_3D)
        },
        UniformValue::IntegralTexture3d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_3D)
        },
        UniformValue::UnsignedTexture3d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_3D)
        },
        UniformValue::DepthTexture3d(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_3D)
        },
        UniformValue::Texture1dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D_ARRAY)
        },
        UniformValue::CompressedTexture1dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D_ARRAY)
        },
        UniformValue::SrgbTexture1dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D_ARRAY)
        },
        UniformValue::CompressedSrgbTexture1dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D_ARRAY)
        },
        UniformValue::IntegralTexture1dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D_ARRAY)
        },
        UniformValue::UnsignedTexture1dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D_ARRAY)
        },
        UniformValue::DepthTexture1dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_1D_ARRAY)
        },
        UniformValue::Texture2dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_ARRAY)
        },
        UniformValue::CompressedTexture2dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_ARRAY)
        },
        UniformValue::SrgbTexture2dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_ARRAY)
        },
        UniformValue::CompressedSrgbTexture2dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_ARRAY)
        },
        UniformValue::IntegralTexture2dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_ARRAY)
        },
        UniformValue::UnsignedTexture2dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_ARRAY)
        },
        UniformValue::DepthTexture2dArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_ARRAY)
        },
        UniformValue::Texture2dMultisampleArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE_ARRAY)
        },
        UniformValue::SrgbTexture2dMultisampleArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE_ARRAY)
        },
        UniformValue::IntegralTexture2dMultisampleArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE_ARRAY)
        },
        UniformValue::UnsignedTexture2dMultisampleArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE_ARRAY)
        },
        UniformValue::DepthTexture2dMultisampleArray(texture, sampler) => {
            let texture = texture.get_id();
            bind_texture_uniform(ctxt, samplers, texture, sampler, location, program, texture_bind_points, gl::TEXTURE_2D_MULTISAMPLE_ARRAY)
        },
    }
}

fn bind_texture_uniform<P>(mut ctxt: &mut context::CommandContext,
                           samplers: &mut HashMap<SamplerBehavior, SamplerObject>,
                           texture: gl::types::GLuint,
                           sampler: Option<SamplerBehavior>, location: gl::types::GLint,
                           program: &P,
                           texture_bind_points: &mut Bitsfield,
                           bind_point: gl::types::GLenum)
                           -> Result<(), DrawError> where P: ProgramExt
{
    let sampler = if let Some(sampler) = sampler {
        Some(try!(::sampler_object::get_sampler(ctxt, samplers, &sampler)))
    } else {
        None
    };

    let sampler = sampler.unwrap_or(0);

    // finding an appropriate texture unit
    let texture_unit =
        ctxt.state.texture_units
            .iter().enumerate()
            .find(|&(unit, content)| {
                content.texture == texture && (content.sampler == sampler ||
                                               !texture_bind_points.is_used(unit as u16))
            })
            .map(|(unit, _)| unit as u16)
            .or_else(|| {
                if ctxt.state.texture_units.len() <
                    ctxt.capabilities.max_combined_texture_image_units as usize
                {
                    Some(ctxt.state.texture_units.len() as u16)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| {
                texture_bind_points.get_unused().expect("Not enough texture units available")
            });
    assert!((texture_unit as gl::types::GLint) <
            ctxt.capabilities.max_combined_texture_image_units);
    texture_bind_points.set_used(texture_unit);

    // updating the program to use the right unit
    program.set_uniform(ctxt, location,
                        &RawUniformValue::SignedInt(texture_unit as gl::types::GLint));

    // updating the state of the texture unit
    if ctxt.state.texture_units.len() <= texture_unit as usize {
        for _ in (ctxt.state.texture_units.len() .. texture_unit as usize + 1) {
            ctxt.state.texture_units.push(Default::default());
        }
    }

    if ctxt.state.texture_units[texture_unit as usize].texture != texture ||
       ctxt.state.texture_units[texture_unit as usize].sampler != sampler
    {
        // TODO: what if it's not supported?
        if ctxt.state.active_texture != texture_unit as gl::types::GLenum {
            unsafe { ctxt.gl.ActiveTexture(texture_unit as gl::types::GLenum + gl::TEXTURE0) };
            ctxt.state.active_texture = texture_unit as gl::types::GLenum;
        }

        if ctxt.state.texture_units[texture_unit as usize].texture != texture {
            unsafe { ctxt.gl.BindTexture(bind_point, texture); }
            ctxt.state.texture_units[texture_unit as usize].texture = texture;
        }

        if ctxt.state.texture_units[texture_unit as usize].sampler != sampler {
            assert!(ctxt.version >= &Version(Api::Gl, 3, 3) ||
                    ctxt.extensions.gl_arb_sampler_objects);

            unsafe { ctxt.gl.BindSampler(texture_unit as gl::types::GLenum, sampler); }
            ctxt.state.texture_units[texture_unit as usize].sampler = sampler;
        }
    }

    Ok(())
}
