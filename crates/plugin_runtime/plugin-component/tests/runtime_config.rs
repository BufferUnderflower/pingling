use pingling_host_contract::{
    ComponentDescriptor, ComponentFunctionDescriptor, ComponentInterfaceDescriptor,
    ComponentRecordDescriptor, IpcPackageDescriptor, WitFieldDescriptor, WitResultDescriptor,
};
use pingling_plugin_component::{
    ComponentPackageRuntime, ComponentRuntimeConfig, DEFAULT_WASMTIME_TARGET,
};

fn demo_package() -> IpcPackageDescriptor {
    let mut package = IpcPackageDescriptor::new("example.demo-user").unwrap();
    package.component = Some(ComponentDescriptor {
        wit_package: "example:user-plugin".to_owned(),
        world: "plugin".to_owned(),
        imports: vec![ComponentInterfaceDescriptor {
            name: "events".to_owned(),
            external: None,
            records: vec![ComponentRecordDescriptor {
                name: "user-changed".to_owned(),
                fields: vec![WitFieldDescriptor {
                    name: "username".to_owned(),
                    ty: "string".to_owned(),
                }],
            }],
            functions: vec![ComponentFunctionDescriptor {
                name: "emit-user-changed".to_owned(),
                params: vec![WitFieldDescriptor {
                    name: "event".to_owned(),
                    ty: "user-changed".to_owned(),
                }],
                result: None,
            }],
        }],
        exports: vec![ComponentInterfaceDescriptor {
            name: "user-api".to_owned(),
            external: None,
            records: vec![ComponentRecordDescriptor {
                name: "login-request".to_owned(),
                fields: vec![WitFieldDescriptor {
                    name: "email".to_owned(),
                    ty: "string".to_owned(),
                }],
            }],
            functions: vec![ComponentFunctionDescriptor {
                name: "login".to_owned(),
                params: vec![WitFieldDescriptor {
                    name: "req".to_owned(),
                    ty: "login-request".to_owned(),
                }],
                result: Some(WitResultDescriptor {
                    ok: Some("string".to_owned()),
                    err: Some("string".to_owned()),
                }),
            }],
        }],
    });
    package
}

#[test]
fn runtime_defaults_to_pulley64() {
    let config = ComponentRuntimeConfig::default();

    assert_eq!(DEFAULT_WASMTIME_TARGET, "pulley64");
    assert_eq!(config.target(), "pulley64");
    config
        .wasmtime_config()
        .expect("pulley64 target is supported");
}

#[test]
fn package_runtime_renders_wit_from_descriptor() {
    let runtime = ComponentPackageRuntime::prepare(demo_package()).unwrap();

    assert_eq!(runtime.package_id(), "example.demo-user");
    assert_eq!(runtime.config().target(), "pulley64");
    assert!(runtime.wit().contains("package example:user-plugin;"));
    assert!(runtime.wit().contains("world plugin"));
    assert!(runtime.wit().contains("export user-api;"));
}

#[test]
fn package_runtime_rejects_packages_without_component_descriptor() {
    let package = IpcPackageDescriptor::new("example.empty").unwrap();

    let error = ComponentPackageRuntime::prepare(package).unwrap_err();

    assert!(error.contains("missing component descriptor"));
}
