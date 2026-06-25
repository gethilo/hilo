use hilo_plugins::{HostFunctions, PluginRegistry, PluginRuntime};

#[test]
fn test_host_function_call() {
    let mut hf = HostFunctions::new();
    hf.file_store.insert("test.txt".into(), "hello".into());
    let result = hf
        .call_host_function("get_file_content", &["test.txt".into()])
        .unwrap();
    assert_eq!(result, "hello");
}

#[test]
fn test_host_function_unknown() {
    let mut hf = HostFunctions::new();
    let result = hf.call_host_function("nonexistent", &[]);
    assert!(result.is_err());
}

#[test]
fn test_registry_discover_empty_dir() {
    let dir = std::env::temp_dir().join("hilo_empty_plugins_test");
    let _ = std::fs::create_dir_all(&dir);
    let manifests = PluginRegistry::discover(&dir).unwrap();
    assert!(manifests.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_registry_discover_wasm_files() {
    let dir = std::env::temp_dir().join("hilo_plugins_test");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("scanner.wasm"), b"mock wasm").unwrap();
    std::fs::write(dir.join("linter.wasm"), b"mock wasm").unwrap();
    let manifests = PluginRegistry::discover(&dir).unwrap();
    assert_eq!(manifests.len(), 2);
    let names: Vec<&str> = manifests.iter().map(|m| m.name.as_str()).collect();
    assert!(names.contains(&"scanner"));
    assert!(names.contains(&"linter"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_runtime_load_plugin() {
    let mut rt = PluginRuntime::new();
    let dir = std::env::temp_dir().join("hilo_runtime_test");
    let _ = std::fs::create_dir_all(&dir);
    let wasm_path = dir.join("test_plugin.wasm");
    std::fs::write(&wasm_path, b"").unwrap();
    let result = rt.load_plugin(&wasm_path);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), "test_plugin");
    assert_eq!(rt.plugins.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_host_functions_add_edge_and_warning() {
    let mut hf = HostFunctions::new();
    hf.call_host_function(
        "add_edge",
        &["a.go".into(), "b.go".into(), "imports".into()],
    )
    .unwrap();
    assert_eq!(hf.edges.len(), 1);
    assert_eq!(
        hf.edges[0],
        ("a.go".into(), "b.go".into(), "imports".into())
    );

    hf.call_host_function("emit_warning", &["main.rs".into(), "unsafe block".into()])
        .unwrap();
    assert_eq!(hf.warnings.len(), 1);
    assert_eq!(hf.warnings[0], ("main.rs".into(), "unsafe block".into()));
}

#[test]
fn test_host_functions_set_xattr() {
    let mut hf = HostFunctions::new();
    hf.call_host_function(
        "set_xattr",
        &["file.go".into(), "user.vfs.feature".into(), "auth".into()],
    )
    .unwrap();
    let result = hf
        .call_host_function("get_xattr", &["file.go".into(), "user.vfs.feature".into()])
        .unwrap();
    assert_eq!(result, "auth");
}

#[test]
fn test_host_functions_get_file_missing() {
    let mut hf = HostFunctions::new();
    // Missing file returns empty string, not error.
    let result = hf
        .call_host_function("get_file_content", &["nonexistent.txt".into()])
        .unwrap();
    assert_eq!(result, "");
}

#[test]
fn test_host_functions_query_graph_stub() {
    let mut hf = HostFunctions::new();
    let result = hf
        .call_host_function("query_graph", &["SELECT * FROM edges".into()])
        .unwrap();
    assert_eq!(result, "[]");
}

#[test]
fn test_runtime_unload_plugin() {
    let mut rt = PluginRuntime::new();
    let dir = std::env::temp_dir().join("hilo_unload_test");
    let _ = std::fs::create_dir_all(&dir);

    std::fs::write(dir.join("alpha.wasm"), b"").unwrap();
    std::fs::write(dir.join("beta.wasm"), b"").unwrap();
    rt.load_plugin(&dir.join("alpha.wasm")).unwrap();
    rt.load_plugin(&dir.join("beta.wasm")).unwrap();
    assert_eq!(rt.plugins.len(), 2);

    assert!(rt.unload_plugin("alpha"));
    assert_eq!(rt.plugins.len(), 1);
    assert_eq!(rt.plugins[0].name, "beta");

    // Unloading a non-existent plugin returns false.
    assert!(!rt.unload_plugin("gamma"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_runtime_dispatch_hook() {
    let mut rt = PluginRuntime::new();
    let dir = std::env::temp_dir().join("hilo_dispatch_test");
    let _ = std::fs::create_dir_all(&dir);

    std::fs::write(dir.join("scanner.wasm"), b"").unwrap();
    rt.load_plugin(&dir.join("scanner.wasm")).unwrap();

    // dispatch_hook for file_write — the loaded plugin has edge_type "tested_by"
    // and a file_write hook, so we expect AddEdge + Warning.
    let results = rt.dispatch_hook("file_write", "main.rs", "file content");

    assert!(
        results.iter().any(|r| matches!(
            r,
            hilo_plugins::HookResult::AddEdge {
                from,
                to,
                relation
            } if from == "main.rs" && to == "test_target" && relation == "tested_by"
        )),
        "expected AddEdge result for tested_by"
    );
    assert!(
        results.iter().any(|r| matches!(
            r,
            hilo_plugins::HookResult::Warning { path, .. } if path == "main.rs"
        )),
        "expected Warning result"
    );

    // No matching hook for "file_read" since the plugin only has file_write.
    let no_results = rt.dispatch_hook("file_read", "main.rs", "");
    assert!(no_results.is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_runtime_host_functions_mut() {
    let mut rt = PluginRuntime::new();
    rt.host_functions_mut()
        .file_store
        .insert("hello.txt".into(), "world".into());
    let result = rt
        .host_functions_mut()
        .call_host_function("get_file_content", &["hello.txt".into()])
        .unwrap();
    assert_eq!(result, "world");
}

#[test]
fn test_registry_discover_nonexistent_dir() {
    // Non-existent directory returns empty vec, not an error.
    let manifests =
        PluginRegistry::discover(std::path::Path::new("/nonexistent/path/xyz")).unwrap();
    assert!(manifests.is_empty());
}
