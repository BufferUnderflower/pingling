use pingling_host_contract::{
    render_wit_world, ComponentDescriptor, ComponentFunctionDescriptor,
    ComponentInterfaceDescriptor, ComponentRecordDescriptor, IpcPackageDescriptor,
    WitFieldDescriptor, WitResultDescriptor,
};

#[test]
fn renders_user_plugin_world_from_component_descriptor() {
    let mut package = IpcPackageDescriptor::new("pingle.user-demo").unwrap();
    package.component = Some(ComponentDescriptor {
        wit_package: "pingle:user-plugin".to_owned(),
        world: "plugin".to_owned(),
        imports: vec![ComponentInterfaceDescriptor {
            name: "events".to_owned(),
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
            records: vec![
                ComponentRecordDescriptor {
                    name: "login-request".to_owned(),
                    fields: vec![
                        WitFieldDescriptor {
                            name: "email".to_owned(),
                            ty: "string".to_owned(),
                        },
                        WitFieldDescriptor {
                            name: "password".to_owned(),
                            ty: "string".to_owned(),
                        },
                    ],
                },
                ComponentRecordDescriptor {
                    name: "login-response".to_owned(),
                    fields: vec![
                        WitFieldDescriptor {
                            name: "token".to_owned(),
                            ty: "string".to_owned(),
                        },
                        WitFieldDescriptor {
                            name: "username".to_owned(),
                            ty: "string".to_owned(),
                        },
                    ],
                },
            ],
            functions: vec![
                ComponentFunctionDescriptor {
                    name: "login".to_owned(),
                    params: vec![WitFieldDescriptor {
                        name: "req".to_owned(),
                        ty: "login-request".to_owned(),
                    }],
                    result: Some(WitResultDescriptor {
                        ok: Some("login-response".to_owned()),
                        err: Some("string".to_owned()),
                    }),
                },
                ComponentFunctionDescriptor {
                    name: "get-username".to_owned(),
                    params: vec![],
                    result: Some(WitResultDescriptor {
                        ok: Some("string".to_owned()),
                        err: Some("string".to_owned()),
                    }),
                },
            ],
        }],
    });

    package.validate().unwrap();
    let wit = render_wit_world(package.component.as_ref().unwrap()).unwrap();

    assert!(wit.contains("package pingle:user-plugin;"));
    assert!(wit.contains("interface events {"));
    assert!(wit.contains("record user-changed {"));
    assert!(wit.contains("emit-user-changed: func(event: user-changed);"));
    assert!(wit.contains("interface user-api {"));
    assert!(wit.contains("login: func(req: login-request) -> result<login-response, string>;"));
    assert!(wit.contains("get-username: func() -> result<string, string>;"));
    assert!(wit.contains("world plugin {"));
    assert!(wit.contains("import events;"));
    assert!(wit.contains("export user-api;"));
}
