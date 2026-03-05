#version 450 core

// Vertex attribute inputs
layout(location = 0) in vec3 aPosition;
layout(location = 1) in vec3 aNormal;
layout(location = 2) in vec2 aTexCoord;

// Outputs to fragment shader
layout(location = 0) out vec3 vWorldPos;
layout(location = 1) out vec3 vNormal;
layout(location = 2) out vec2 vTexCoord;
layout(location = 3) out vec4 vShadowCoord;

// Uniform buffer for transform matrices
layout(std140, binding = 0) uniform Matrices {
    mat4 model;
    mat4 view;
    mat4 projection;
    mat4 lightSpaceMatrix;
};

// Material properties
struct Material {
    vec3 ambient;
    vec3 diffuse;
    vec3 specular;
    float shininess;
};

// Point light
struct PointLight {
    vec3 position;
    vec3 color;
    float intensity;
    float radius;
};

#define MAX_LIGHTS 8
#define PI 3.14159265359

/// Transform normal from object to world space
vec3 transformNormal(vec3 normal, mat4 modelMatrix) {
    mat3 normalMatrix = transpose(inverse(mat3(modelMatrix)));
    return normalize(normalMatrix * normal);
}

/// Compute fresnel factor using Schlick approximation
float fresnelSchlick(float cosTheta, float F0) {
    return F0 + (1.0 - F0) * pow(clamp(1.0 - cosTheta, 0.0, 1.0), 5.0);
}

/// Compute distance attenuation for point lights
float attenuate(float distance, float radius) {
    float d = max(distance, 0.001);
    float attenuation = 1.0 / (d * d);
    float falloff = clamp(1.0 - pow(d / radius, 4.0), 0.0, 1.0);
    return attenuation * falloff;
}

/// Apply fog effect based on distance
vec4 applyFog(vec4 color, float distance, vec3 fogColor, float fogDensity) {
    float fogFactor = exp(-fogDensity * distance);
    fogFactor = clamp(fogFactor, 0.0, 1.0);
    return mix(vec4(fogColor, 1.0), color, fogFactor);
}

void main() {
    vec4 worldPos = model * vec4(aPosition, 1.0);
    vWorldPos = worldPos.xyz;
    vNormal = transformNormal(aNormal, model);
    vTexCoord = aTexCoord;
    vShadowCoord = lightSpaceMatrix * worldPos;

    gl_Position = projection * view * worldPos;
}
