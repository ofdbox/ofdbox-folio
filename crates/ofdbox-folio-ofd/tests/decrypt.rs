//! Reading tests for encrypted OFD files.

use std::io::{Cursor, Write};

use ofdbox_crypto::{EncryptOptions, encrypt_package};
use ofdbox_folio_ir::DocumentSource;
use ofdbox_folio_ofd::{OfdDocument, OfdReadOptions};

fn plain_ofd() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    let entries = [
        (
            "OFD.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:OFD xmlns:ofd="http://www.ofdspec.org/2016" Version="1.0" DocType="OFD">
  <ofd:DocBody>
    <ofd:DocInfo><ofd:DocID>encrypted</ofd:DocID></ofd:DocInfo>
    <ofd:DocRoot>Doc_0/Document.xml</ofd:DocRoot>
  </ofd:DocBody>
</ofd:OFD>"#,
        ),
        (
            "Doc_0/Document.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Document xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:CommonData>
    <ofd:MaxUnitID>3</ofd:MaxUnitID>
    <ofd:PageArea><ofd:PhysicalBox>0 0 100 100</ofd:PhysicalBox></ofd:PageArea>
  </ofd:CommonData>
  <ofd:Pages><ofd:Page ID="1" BaseLoc="Pages/Page_0/Content.xml"/></ofd:Pages>
</ofd:Document>"#,
        ),
        (
            "Doc_0/Pages/Page_0/Content.xml",
            r#"<?xml version="1.0" encoding="UTF-8"?>
<ofd:Page xmlns:ofd="http://www.ofdspec.org/2016">
  <ofd:Area><ofd:PhysicalBox>0 0 100 100</ofd:PhysicalBox></ofd:Area>
  <ofd:Content>
    <ofd:Layer ID="2">
      <ofd:PathObject ID="3" Boundary="0 0 10 10" Fill="true" Stroke="false">
        <ofd:FillColor Value="255 0 0"/>
        <ofd:AbbreviatedData>M 0 0 L 10 0 L 10 10 L 0 10 C</ofd:AbbreviatedData>
      </ofd:PathObject>
    </ofd:Layer>
  </ofd:Content>
</ofd:Page>"#,
        ),
    ];
    for (name, content) in entries {
        zip.start_file(name, options).expect("zip entry");
        zip.write_all(content.as_bytes()).expect("zip write");
    }
    zip.finish().expect("zip finish").into_inner()
}

#[test]
fn opens_password_encrypted_ofd() {
    let plain = plain_ofd();
    let encrypted = encrypt_package(&plain, &EncryptOptions::password("alice", "secret"))
        .expect("encrypt package");
    let options = OfdReadOptions {
        password: Some("secret".to_owned()),
        ..OfdReadOptions::default()
    };
    let mut document = OfdDocument::open_bytes(encrypted, options).expect("open encrypted");
    assert_eq!(document.page_count(), 1);
    let page = document.load_page(0).expect("load page");
    assert_eq!(page.content.size.width, 100.0);
}

#[test]
fn encrypted_without_credentials_is_rejected() {
    let plain = plain_ofd();
    let encrypted = encrypt_package(&plain, &EncryptOptions::password("alice", "secret"))
        .expect("encrypt package");
    let error = match OfdDocument::open_bytes(encrypted, OfdReadOptions::default()) {
        Ok(_) => panic!("encrypted without password must fail"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("encrypted"),
        "unexpected error: {error}"
    );
}

#[test]
fn wrong_password_is_rejected() {
    let plain = plain_ofd();
    let encrypted = encrypt_package(&plain, &EncryptOptions::password("alice", "secret"))
        .expect("encrypt package");
    let options = OfdReadOptions {
        password: Some("wrong".to_owned()),
        ..OfdReadOptions::default()
    };
    assert!(OfdDocument::open_bytes(encrypted, options).is_err());
}
