//! macOS: Finder Quick Actions as per-user Automator workflows in `~/Library/Services`
//! (a Run Shell Script action calling the app), and the default app through the
//! `DefaultApps` the app supplies (LaunchServices; the system asks the user).

use std::path::Path;

use pagebara_identity::NAME;

use crate::catalog::{MenuAction, menu_actions_of};
use crate::env::Env;
use crate::linux::{sh_quote, xml_escape};
use crate::plan::Op;
use crate::{NoPlan, Os};

const PDF_UTI: &str = "com.adobe.pdf";
const IMAGE_UTIS: [&str; 2] = ["public.png", "public.jpeg"];

fn info_plist(title: &str, types: &[&str]) -> String {
    let types: String = types
        .iter()
        .map(|t| format!("\t\t\t\t<string>{t}</string>\n"))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>NSServices</key>
	<array>
		<dict>
			<key>NSMenuItem</key>
			<dict>
				<key>default</key>
				<string>{}</string>
			</dict>
			<key>NSMessage</key>
			<string>runWorkflowAsService</string>
			<key>NSRequiredContext</key>
			<dict>
				<key>NSApplicationIdentifier</key>
				<string>com.apple.finder</string>
			</dict>
			<key>NSSendFileTypes</key>
			<array>
{types}			</array>
		</dict>
	</array>
</dict>
</plist>
"#,
        xml_escape(title)
    )
}

/// An Automator document with one Run Shell Script action taking the files as arguments.
fn document(command: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>AMApplicationBuild</key>
	<string>523</string>
	<key>AMApplicationVersion</key>
	<string>2.10</string>
	<key>AMDocumentVersion</key>
	<string>2</string>
	<key>actions</key>
	<array>
		<dict>
			<key>action</key>
			<dict>
				<key>AMAccepts</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Optional</key>
					<true/>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>AMActionVersion</key>
				<string>2.0.3</string>
				<key>AMApplication</key>
				<array>
					<string>Automator</string>
				</array>
				<key>AMParameterProperties</key>
				<dict>
					<key>COMMAND_STRING</key>
					<dict/>
					<key>CheckedForUserDefaultShell</key>
					<dict/>
					<key>inputMethod</key>
					<dict/>
					<key>shell</key>
					<dict/>
					<key>source</key>
					<dict/>
				</dict>
				<key>AMProvides</key>
				<dict>
					<key>Container</key>
					<string>List</string>
					<key>Types</key>
					<array>
						<string>com.apple.cocoa.string</string>
					</array>
				</dict>
				<key>ActionBundlePath</key>
				<string>/System/Library/Automator/Run Shell Script.action</string>
				<key>ActionName</key>
				<string>Run Shell Script</string>
				<key>ActionParameters</key>
				<dict>
					<key>COMMAND_STRING</key>
					<string>{}</string>
					<key>CheckedForUserDefaultShell</key>
					<true/>
					<key>inputMethod</key>
					<integer>1</integer>
					<key>shell</key>
					<string>/bin/sh</string>
					<key>source</key>
					<string></string>
				</dict>
				<key>BundleIdentifier</key>
				<string>com.apple.RunShellScript</string>
				<key>CFBundleVersion</key>
				<string>2.0.3</string>
				<key>CanShowSelectedItemsWhenRun</key>
				<false/>
				<key>CanShowWhenRun</key>
				<true/>
				<key>Class Name</key>
				<string>RunShellScriptAction</string>
				<key>InputUUID</key>
				<string>8A4B2C6E-1D3F-4E5A-9B7C-0D1E2F3A4B5C</string>
				<key>OutputUUID</key>
				<string>9B5C3D7F-2E4A-4F6B-8C9D-1E2F3A4B5C6D</string>
				<key>UUID</key>
				<string>7C3A1B5D-0C2E-4D49-8A6B-9C0D1E2F3A4B</string>
				<key>isViewVisible</key>
				<integer>1</integer>
			</dict>
		</dict>
	</array>
	<key>connectors</key>
	<dict/>
	<key>workflowMetaData</key>
	<dict>
		<key>serviceApplicationBundleID</key>
		<string>com.apple.finder</string>
		<key>serviceApplicationPath</key>
		<string>/System/Library/CoreServices/Finder.app</string>
		<key>serviceInputTypeIdentifier</key>
		<string>com.apple.Automator.fileSystemObject</string>
		<key>serviceOutputTypeIdentifier</key>
		<string>com.apple.Automator.nothing</string>
		<key>serviceProcessesInput</key>
		<integer>0</integer>
		<key>workflowTypeIdentifier</key>
		<string>com.apple.Automator.servicesMenu</string>
	</dict>
</dict>
</plist>
"#,
        xml_escape(command)
    )
}

fn command(exe: &Path, args: &[&str]) -> String {
    let mut c = format!("exec {}", sh_quote(&exe.to_string_lossy()));
    for a in args {
        c.push(' ');
        c.push_str(&sh_quote(a));
    }
    c.push_str(" \"$@\"");
    c
}

/// The two files of a Quick Action bundle.
fn quick_action(env: &Env, title: &str, args: &[&str], types: &[&str]) -> Vec<Op> {
    let dir = env
        .home
        .join("Library/Services")
        .join(format!("{title} ({NAME}).workflow/Contents"));
    vec![
        Op::File {
            path: dir.join("Info.plist"),
            contents: info_plist(&format!("{title} ({NAME})"), types).into_bytes(),
            executable: false,
        },
        Op::File {
            path: dir.join("document.wflow"),
            contents: document(&command(&env.exe, args)).into_bytes(),
            executable: false,
        },
    ]
}

pub fn plan(env: &Env, id: &str) -> Result<Vec<Op>, NoPlan> {
    Ok(match id {
        "open-with" => {
            return Err(NoPlan::Provided(pagebara_i18n::tr!(
                "Declared by the app bundle; macOS lists the app under “Open With”."
            )));
        }
        "default-pdf" => {
            env.defaults.available().map_err(NoPlan::Unavailable)?;
            let app = env.defaults.ours().ok_or_else(|| {
                NoPlan::Unavailable(pagebara_i18n::tr!(
                    "Start the app from its .app bundle to make it the default."
                ))
            })?;
            vec![Op::Default {
                types: vec![PDF_UTI.into()],
                app,
            }]
        }
        "type-images" => quick_action(env, "Convert to PDF", &["--action", "convert"], &IMAGE_UTIS),
        _ if id.starts_with("action-") => {
            let actions: Vec<&MenuAction> = menu_actions_of(id);
            if actions.is_empty() {
                return Err(NoPlan::Unavailable("unknown item".into()));
            }
            actions
                .into_iter()
                .flat_map(|a| quick_action(env, a.title, a.args, &[PDF_UTI]))
                .collect()
        }
        _ => {
            return Err(NoPlan::Unavailable(pagebara_i18n::translate(
                crate::catalog::unavailable(Os::MacOs, id)
                    .unwrap_or(pagebara_i18n::tr_noop!("Not available on macOS.")),
                &[],
            )));
        }
    })
}
