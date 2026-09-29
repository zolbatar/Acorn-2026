struct OutputParameters {
    colour_mode: u32,
    target_is_srgb: u32,
    _padding: vec2<u32>,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var palette_lookup: texture_3d<f32>;
@group(0) @binding(2) var<uniform> parameters: OutputParameters;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    let vertices = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    var output: VertexOutput;
    output.position = vec4<f32>(vertices[vertex_index], 0.0, 1.0);
    return output;
}

fn srgb_to_linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        return channel / 12.92;
    }
    return pow((channel + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb(channel: f32) -> f32 {
    if channel <= 0.0031308 {
        return channel * 12.92;
    }
    return 1.055 * pow(channel, 1.0 / 2.4) - 0.055;
}

fn decode_srgb(value: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        srgb_to_linear(value.r),
        srgb_to_linear(value.g),
        srgb_to_linear(value.b),
    );
}

fn encode_srgb(value: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        linear_to_srgb(value.r),
        linear_to_srgb(value.g),
        linear_to_srgb(value.b),
    );
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let pixel = vec2<i32>(position.xy);
    let input = textureLoad(source, pixel, 0);

    var linear_colour = decode_srgb(input.rgb);
    var encoded_colour = input.rgb;
    if parameters.target_is_srgb != 0u {
        // Sampling an sRGB view has already decoded the Vello output.
        linear_colour = input.rgb;
        encoded_colour = encode_srgb(linear_colour);
    }

    let luminance = dot(linear_colour, vec3<f32>(0.2126, 0.7152, 0.0722));
    let grey_code = linear_to_srgb(luminance);

    switch parameters.colour_mode {
        case 0u: {
            let bit = select(0.0, 1.0, luminance >= 0.5);
            encoded_colour = vec3<f32>(bit);
        }
        case 1u: {
            encoded_colour = vec3<f32>(round(grey_code * 3.0) / 3.0);
        }
        case 2u: {
            encoded_colour = vec3<f32>(round(grey_code * 15.0) / 15.0);
        }
        case 3u, 5u: {
            let cell = vec3<i32>(min(floor(encoded_colour * 32.0), vec3<f32>(31.0)));
            encoded_colour = textureLoad(palette_lookup, cell, 0).rgb;
        }
        case 4u: {
            encoded_colour = vec3<f32>(round(grey_code * 255.0) / 255.0);
        }
        case 6u: {
            encoded_colour = round(encoded_colour * 31.0) / 31.0;
        }
        default: {
            // RGB888 deliberately preserves Vello's encoded channel values.
        }
    }

    var output_colour = encoded_colour;
    if parameters.target_is_srgb != 0u {
        // Let the sRGB render target encode the already-quantized code values.
        output_colour = decode_srgb(encoded_colour);
    }
    return vec4<f32>(output_colour, input.a);
}
