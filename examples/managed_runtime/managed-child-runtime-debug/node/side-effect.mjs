// Set one global marker to prove side-effect imports execute exactly during module loading.
// 设置一个全局标记，用于证明副作用 import 会在模块加载时执行。
globalThis.__luaskillsSideEffectMarker = "side-effect";
