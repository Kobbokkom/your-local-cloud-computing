import Foundation
import Metal

func fail(_ text: String) -> Never { fputs(text + "\n", stderr); exit(1) }
guard let device = MTLCreateSystemDefaultDevice(), let queue = device.makeCommandQueue() else { fail("Metal device unavailable") }
let source = """
#include <metal_stdlib>
using namespace metal;
kernel void add(device const float *a [[buffer(0)]], device const float *b [[buffer(1)]], device float *out [[buffer(2)]], uint id [[thread_position_in_grid]]) { out[id] = a[id] + b[id]; }
"""
let library = try device.makeLibrary(source: source, options: nil)
guard let function = library.makeFunction(name: "add") else { fail("missing GPU function") }
let pipeline = try device.makeComputePipelineState(function: function)
let count = 4096
let a = (0..<count).map { Float($0) }
let b = (0..<count).map { Float($0 * 2) }
let bytes = count * MemoryLayout<Float>.stride
let inputA = a.withUnsafeBytes { device.makeBuffer(bytes: $0.baseAddress!, length: bytes, options: .storageModeShared)! }
let inputB = b.withUnsafeBytes { device.makeBuffer(bytes: $0.baseAddress!, length: bytes, options: .storageModeShared)! }
guard let output = device.makeBuffer(length: bytes, options: .storageModeShared), let command = queue.makeCommandBuffer(), let encoder = command.makeComputeCommandEncoder() else { fail("cannot create GPU command") }
encoder.setComputePipelineState(pipeline)
encoder.setBuffer(inputA, offset: 0, index: 0)
encoder.setBuffer(inputB, offset: 0, index: 1)
encoder.setBuffer(output, offset: 0, index: 2)
encoder.dispatchThreads(MTLSize(width: count, height: 1, depth: 1), threadsPerThreadgroup: MTLSize(width: min(count, pipeline.maxTotalThreadsPerThreadgroup), height: 1, depth: 1))
encoder.endEncoding()
command.commit()
command.waitUntilCompleted()
guard command.status == .completed else { fail("GPU execution failed: \(String(describing: command.error))") }
let result = output.contents().bindMemory(to: Float.self, capacity: count)
for i in 0..<count { guard result[i] == Float(i * 3) else { fail("incorrect GPU output at \(i)") } }
let report: [String: Any] = ["backend": "metal", "device": device.name, "count": count, "checksum": (0..<count).reduce(0) { $0 + Int(result[$1]) }]
let json = try JSONSerialization.data(withJSONObject: report, options: [.sortedKeys])
try json.write(to: URL(fileURLWithPath: "result.json"))
print(String(data: json, encoding: .utf8)!)
