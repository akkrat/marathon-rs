//! Port of core/src/main/kotlin/com/malinskiy/marathon/report/html/HtmlSummaryReporter.kt
//! and report/allure/AllureReporter.kt (Commons Text and Allure XML boundaries).

/// Commons Text 1.12 escapeXml11, including removal of invalid XML code points.
pub fn escape_xml11(text: &str) -> String {
    let mut out = String::default();
    for ch in text.chars() {
        match ch {
            '\0' | '\u{fffe}' | '\u{ffff}' => {}
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\u{1}'..='\u{8}' | '\u{b}'..='\u{c}' | '\u{e}'..='\u{1f}' | '\u{7f}'..='\u{84}' | '\u{86}'..='\u{9f}' => {
                out.push_str("&#");
                out.push_str(&(ch as u32).to_string());
                out.push(';');
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Java URLEncoder with UTF-8; spaces become plus, not percent-20.
pub fn form_encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

pub fn replace(text: &str, pattern: &str, replacement: &str) -> String {
    text.replace(pattern, replacement)
}

/// ResultsUtils.generateMethodSignatureHash updates MD5 with class bytes then method bytes (no separators).
pub fn allure_history_id(class: &str, method: &str) -> String {
    let mut signature = class.to_owned();
    signature.push_str(method);
    format!("{:x}", md5::compute(signature.as_bytes()))
}

/// Why the JDK's Xalan serializer rejects a text node (environment-writer catches it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmlTextError {
    /// SAXException: a control character.
    InvalidCharacter(u16),
}
impl XmlTextError {
    /// The SAXException message.
    pub fn sax_message(self) -> String {
        match self {
            Self::InvalidCharacter(u) => {
                format!("An invalid XML character (Unicode: 0x{u:x}) was found in the node's character data content.")
            }
        }
    }
}

/// DOM text-node serialization used by environment-writer's identity transform.
pub fn xml_text(text: &str) -> Result<String, XmlTextError> {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' | '\n' => out.push(c),
            '\r' | '\u{7f}'..='\u{9f}' | '\u{10000}'..='\u{10ffff}' => out.push_str(&format!("&#{};", c as u32)),
            '\0'..='\u{1f}' => return Err(XmlTextError::InvalidCharacter(c as u16)),
            _ => out.push(c),
        }
    }
    Ok(out)
}
