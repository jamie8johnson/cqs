#include <cuda_runtime.h>
#include <stdio.h>

// Device configuration
struct DeviceConfig {
    int numBlocks;
    int threadsPerBlock;
    size_t sharedMemSize;
};

// Enum for reduction operations
enum class ReductionOp { Sum, Max, Min };

typedef float (*ActivationFn)(float);

#define BLOCK_SIZE 256
#define WARP_SIZE 32
#define CHECK_CUDA(call) do { cudaError_t err = (call); if (err != cudaSuccess) { printf("CUDA error: %s\n", cudaGetErrorString(err)); exit(1); } } while(0)

namespace gpu {

/// Vector addition kernel
__global__ void vectorAdd(const float *a, const float *b, float *c, int n) {
    int idx = blockIdx.x * blockDim.x + threadIdx.x;
    if (idx < n) {
        c[idx] = a[idx] + b[idx];
    }
}

/// Matrix multiply kernel with shared memory
__global__ void matMul(const float *A, const float *B, float *C, int N) {
    __shared__ float tileA[BLOCK_SIZE][BLOCK_SIZE];
    __shared__ float tileB[BLOCK_SIZE][BLOCK_SIZE];

    int row = blockIdx.y * blockDim.y + threadIdx.y;
    int col = blockIdx.x * blockDim.x + threadIdx.x;
    float sum = 0.0f;

    for (int t = 0; t < (N + BLOCK_SIZE - 1) / BLOCK_SIZE; t++) {
        tileA[threadIdx.y][threadIdx.x] = A[row * N + t * BLOCK_SIZE + threadIdx.x];
        tileB[threadIdx.y][threadIdx.x] = B[(t * BLOCK_SIZE + threadIdx.y) * N + col];
        __syncthreads();

        for (int k = 0; k < BLOCK_SIZE; k++) {
            sum += tileA[threadIdx.y][k] * tileB[k][threadIdx.x];
        }
        __syncthreads();
    }

    C[row * N + col] = sum;
}

/// Device helper function
__device__ float sigmoid(float x) {
    return 1.0f / (1.0f + expf(-x));
}

/// Activation kernel using function pointer
__global__ void applyActivation(float *data, int n) {
    int idx = blockIdx.x * blockDim.x + threadIdx.x;
    if (idx < n) {
        data[idx] = sigmoid(data[idx]);
    }
}

} // namespace gpu

/// Host function to launch vector addition
void launchVectorAdd(const float *h_a, const float *h_b, float *h_c, int n) {
    float *d_a, *d_b, *d_c;
    size_t size = n * sizeof(float);

    CHECK_CUDA(cudaMalloc(&d_a, size));
    CHECK_CUDA(cudaMalloc(&d_b, size));
    CHECK_CUDA(cudaMalloc(&d_c, size));

    cudaMemcpy(d_a, h_a, size, cudaMemcpyHostToDevice);
    cudaMemcpy(d_b, h_b, size, cudaMemcpyHostToDevice);

    int blockSize = BLOCK_SIZE;
    int numBlocks = (n + blockSize - 1) / blockSize;
    gpu::vectorAdd<<<numBlocks, blockSize>>>(d_a, d_b, d_c, n);

    cudaMemcpy(h_c, d_c, size, cudaMemcpyDeviceToHost);

    cudaFree(d_a);
    cudaFree(d_b);
    cudaFree(d_c);
}

int main() {
    const int N = 1024;
    float a[N], b[N], c[N];

    for (int i = 0; i < N; i++) {
        a[i] = static_cast<float>(i);
        b[i] = static_cast<float>(i * 2);
    }

    launchVectorAdd(a, b, c, N);

    printf("c[0] = %f, c[N-1] = %f\n", c[0], c[N - 1]);
    return 0;
}
