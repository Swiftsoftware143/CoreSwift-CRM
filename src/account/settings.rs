use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WhiteLabelSettings {
    pub company_name: Option<String>,
    pub tagline: Option<String>,
    pub favicon_url: Option<String>,
    pub login_page_bg: Option<String>,
    pub email_from_name: Option<String>,
    pub email_from_address: Option<String>,
    pub support_email: Option<String>,
    pub support_phone: Option<String>,
    pub terms_url: Option<String>,
    pub privacy_url: Option<String>,
    pub custom_css: Option<String>,
    pub custom_js: Option<String>,
    pub theme: Option<ThemeConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeConfig {
    pub sidebar_bg: Option<String>,
    pub sidebar_text: Option<String>,
    pub header_bg: Option<String>,
    pub button_primary: Option<String>,
    pub button_secondary: Option<String>,
    pub font_family: Option<String>,
    pub border_radius: Option<String>,
}
