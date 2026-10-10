//! Native file selection, using the existing desktop portal on Linux.
use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub async fn pick_file() -> Result<Option<PathBuf>, String> {
    use ashpd::desktop::{ResponseError, file_chooser::SelectedFiles};
    let request = SelectedFiles::open_file()
        .title("Enviar arquivo")
        .accept_label("Selecionar")
        .modal(true)
        .multiple(false)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let files = match request.response() {
        Ok(files) => files,
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    files
        .uris()
        .first()
        .map(|uri| {
            reqwest::Url::parse(uri.as_str())
                .map_err(|error| error.to_string())?
                .to_file_path()
                .map_err(|_| "O seletor não retornou um arquivo local".to_string())
        })
        .transpose()
}

#[cfg(windows)]
pub async fn pick_file() -> Result<Option<PathBuf>, String> {
    tokio::task::spawn_blocking(|| {
        let result = std::process::Command::new("powershell.exe").args([
            "-NoProfile", "-Sta", "-WindowStyle", "Hidden", "-Command",
            "Add-Type -AssemblyName System.Windows.Forms; [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding; $d = New-Object System.Windows.Forms.OpenFileDialog; $d.Title = 'Enviar arquivo'; if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Write($d.FileName) }; $d.Dispose()"
        ]).output().map_err(|error| error.to_string())?;
        if !result.status.success() { return Err("Não foi possível abrir o seletor de arquivos".into()); }
        let path = String::from_utf8(result.stdout).map_err(|error| error.to_string())?;
        Ok((!path.is_empty()).then(|| PathBuf::from(path)))
    }).await.map_err(|error| error.to_string())?
}

#[cfg(not(any(target_os = "linux", windows)))]
pub async fn pick_file() -> Result<Option<PathBuf>, String> {
    Err("Use o campo de caminho para selecionar um arquivo neste sistema".into())
}
