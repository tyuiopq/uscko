// Видео и аудио через встроенный в Windows медиадвижок (Media Foundation,
// API Windows.Media.Transcoding и Windows.Media.Editing). Свои кодеки не
// нужны, размер программы не растёт; аппаратное ускорение — от Windows.
//
// Видео сохраняется в MP4 (H.264 + AAC) или WMV: другие форматы (MOV, MKV,
// WebM) Windows умеет только читать, а HEVC — кодировать лишь с платным
// расширением.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Foundation::TimeSpan;
use windows::Media::Editing::{BackgroundAudioTrack, MediaClip, MediaComposition, MediaTrimmingPreference};
use windows::Media::MediaProperties::{
    AudioEncodingProperties, AudioEncodingQuality, MediaEncodingProfile, VideoEncodingQuality,
};
use windows::Media::Transcoding::{MediaTranscoder, TranscodeFailureReason};
use windows::Storage::StorageFile;
use windows_future::{AsyncActionProgressHandler, AsyncOperationProgressHandler, AsyncStatus};

use crate::actions::ActionError;
use crate::params::Params;
use crate::winrt::{self, winrt_error};
use crate::{job_manager, output};

// Битрейт видео: бит на пиксель кадра (ширина * высота * кадров в секунду).
const CONVERT_BITS_PER_PIXEL: f64 = 0.1;
const SMALL_BITS_PER_PIXEL: f64 = 0.08;
const MAX_VIDEO_BITRATE: f64 = 25_000_000.0;

// Звук в профиле MP4 по умолчанию (AAC), бит/с.
const AUDIO_BITRATE: f64 = 192_000.0;

// Предел AAC-кодировщика Windows, кбит/с.
const M4A_MAX_BITRATE: u32 = 192;

// TimeSpan в Windows — в сотнях наносекунд.
const TICKS_PER_SECOND: f64 = 10_000_000.0;

fn seconds(span: TimeSpan) -> f64 {
    span.Duration as f64 / TICKS_PER_SECOND
}

fn span(seconds: f64) -> TimeSpan {
    TimeSpan {
        Duration: (seconds * TICKS_PER_SECOND).round() as i64,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    Mp4,
    Wmv,
}

impl VideoFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Wmv => "wmv",
        }
    }

    fn profile(self) -> windows::core::Result<MediaEncodingProfile> {
        match self {
            Self::Mp4 => MediaEncodingProfile::CreateMp4(VideoEncodingQuality::HD1080p),
            Self::Wmv => MediaEncodingProfile::CreateWmv(VideoEncodingQuality::HD1080p),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    M4a,
    Wav,
    Flac,
}

impl AudioFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::M4a => "m4a",
            Self::Wav => "wav",
            Self::Flac => "flac",
        }
    }

    // Профиль с битрейтом из параметров (для MP3 и M4A; WAV и FLAC без потерь).
    fn profile(self, params: &Params) -> windows::core::Result<MediaEncodingProfile> {
        let quality = AudioEncodingQuality::High;

        let (profile, kbps) = match self {
            Self::Mp3 => (MediaEncodingProfile::CreateMp3(quality)?, Some(params.audio_bitrate)),

            Self::M4a => (
                MediaEncodingProfile::CreateM4a(quality)?,
                Some(params.audio_bitrate.min(M4A_MAX_BITRATE)),
            ),

            Self::Wav => (MediaEncodingProfile::CreateWav(quality)?, None),
            Self::Flac => (MediaEncodingProfile::CreateFlac(quality)?, None),
        };

        if let Some(kbps) = kbps {
            profile.Audio()?.SetBitrate(kbps * 1000)?;
        }

        Ok(profile)
    }
}

// Ошибка, при которой окно статуса предлагает установить расширение HEVC.
pub const HEVC_MISSING: &str = "Видео в HEVC (как снимает iPhone): Windows нужно расширение";

// Закодировано ли видео в HEVC (H.265) — его Windows открывает только
// с расширением «HEVC Video Extensions» из Microsoft Store.
fn is_hevc(file: &StorageFile) -> bool {
    MediaEncodingProfile::CreateFromFileAsync(file)
        .and_then(|operation| operation.join())
        .and_then(|profile| profile.Video())
        .and_then(|video| video.Subtype())
        .map(|subtype| {
            let subtype = subtype.to_string().to_ascii_uppercase();

            subtype.contains("HEVC") || subtype.contains("H265")
        })
        .unwrap_or(false)
}

fn failure_text(reason: TranscodeFailureReason) -> String {
    match reason {
        // Коротко, чтобы влезло в окно статуса. Чаще всего это видео с iPhone:
        // ему нужно расширение «HEVC Video Extensions» из Microsoft Store.
        TranscodeFailureReason::CodecNotFound => {
            "Нет кодека (для видео с iPhone — расширение HEVC)".to_string()
        }

        TranscodeFailureReason::InvalidProfile => "Не удалось подобрать параметры кодирования".to_string(),

        _ => "Не удалось перекодировать файл".to_string(),
    }
}

fn report_progress(percent: f64) {
    job_manager::set_file_progress(percent / 100.0);
}

// Перекодирует source в destination (пустой временный файл) по профилю.
fn transcode(source: &StorageFile, destination: &Path, profile: &MediaEncodingProfile) -> Result<(), String> {
    let destination = winrt::storage_file(destination)?;

    let transcoder = MediaTranscoder::new().map_err(|error| winrt_error("Не удалось перекодировать", error))?;

    let _ = transcoder.SetHardwareAccelerationEnabled(true);

    let prepared = transcoder
        .PrepareFileTranscodeAsync(source, &destination, profile)
        .and_then(|operation| operation.join())
        .map_err(|error| winrt_error("Не удалось перекодировать", error))?;

    if !prepared.CanTranscode().unwrap_or(false) {
        let reason = prepared
            .FailureReason()
            .unwrap_or(TranscodeFailureReason::Unknown);

        if reason == TranscodeFailureReason::CodecNotFound && is_hevc(source) {
            return Err(HEVC_MISSING.to_string());
        }

        return Err(failure_text(reason));
    }

    let operation = prepared
        .TranscodeAsync()
        .map_err(|error| winrt_error("Не удалось перекодировать", error))?;

    let _ = operation.SetProgress(&AsyncActionProgressHandler::<f64>::new(|_, percent| {
        if let Ok(percent) = percent.ok() {
            report_progress(*percent);
        }

        Ok(())
    }));

    wait_for(|| operation.Status(), || {
        let _ = operation.Cancel();
    })?;

    operation
        .GetResults()
        .map_err(|error| winrt_error("Не удалось перекодировать", error))
}

// Ожидание долгой операции Windows с поддержкой «Отменить».
//
// Не join(): если отменить операцию в самом начале, Windows иногда так и не
// сообщает о её завершении, и ожидание висело бы вечно, вместе с очередью.
// Поэтому опрашиваем статус сами, а после отмены ждём не дольше CANCEL_GRACE.
fn wait_for(
    status: impl Fn() -> windows::core::Result<AsyncStatus>,
    cancel: impl Fn(),
) -> Result<(), String> {
    const POLL: Duration = Duration::from_millis(50);
    const CANCEL_GRACE: Duration = Duration::from_secs(5);

    let mut cancelled_at: Option<Instant> = None;

    loop {
        match status() {
            Ok(AsyncStatus::Started) => {}

            Ok(AsyncStatus::Canceled) => return Err(job_manager::CANCELLED.to_string()),

            // Готово или ошибка: подробности отдаст GetResults().
            Ok(_) => return Ok(()),

            Err(error) => return Err(winrt_error("Не удалось перекодировать", error)),
        }

        if job_manager::is_cancelled() {
            match cancelled_at {
                None => {
                    cancel();

                    cancelled_at = Some(Instant::now());
                }

                Some(since) if since.elapsed() > CANCEL_GRACE => {
                    return Err(job_manager::CANCELLED.to_string());
                }

                Some(_) => {}
            }
        }

        std::thread::sleep(POLL);
    }
}

struct VideoInfo {
    width: u32,
    height: u32,
    fps: (u32, u32),
    has_audio: bool,
}

fn video_info(file: &StorageFile) -> Result<VideoInfo, String> {
    let profile = MediaEncodingProfile::CreateFromFileAsync(file)
        .and_then(|operation| operation.join())
        .map_err(|error| winrt_error("Не удалось прочитать видео", error))?;

    let video = profile
        .Video()
        .map_err(|_| "В файле нет видеодорожки".to_string())?;

    let width = video.Width().unwrap_or(0);
    let height = video.Height().unwrap_or(0);

    if width == 0 || height == 0 {
        return Err("Не удалось определить размер видео".to_string());
    }

    let rate = video.FrameRate().ok();

    let numerator = rate.as_ref().and_then(|rate| rate.Numerator().ok()).unwrap_or(0);
    let denominator = rate.as_ref().and_then(|rate| rate.Denominator().ok()).unwrap_or(0);

    let fps = if numerator > 0 && denominator > 0 {
        (numerator, denominator)
    } else {
        (30, 1)
    };

    Ok(VideoInfo {
        width,
        height,
        fps,
        has_audio: profile.Audio().is_ok(),
    })
}

// Размер не выше max_height с сохранением пропорций; кодеку нужны чётные стороны.
fn fit_height(info: &VideoInfo, max_height: Option<u32>) -> (u32, u32) {
    let height = max_height.map_or(info.height, |max| info.height.min(max));

    let width = (info.width as f64 * height as f64 / info.height as f64).round() as u32;

    (width.max(2) & !1, height.max(2) & !1)
}

// Битрейт видео для размера и частоты кадров, бит/с.
fn video_bitrate(info: &VideoInfo, size: (u32, u32), bits_per_pixel: f64) -> f64 {
    let fps = info.fps.0 as f64 / info.fps.1 as f64;

    (size.0 as f64 * size.1 as f64 * fps * bits_per_pixel).min(MAX_VIDEO_BITRATE)
}

// Профиль под размер, частоту кадров и битрейт.
fn video_profile(
    format: VideoFormat,
    info: &VideoInfo,
    size: (u32, u32),
    bits_per_pixel: f64,
) -> Result<MediaEncodingProfile, String> {
    let profile = format.profile().map_err(|error| winrt_error("Не удалось подобрать параметры кодирования", error))?;

    let video = profile.Video().map_err(|error| winrt_error("Не удалось подобрать параметры кодирования", error))?;

    let bitrate = video_bitrate(info, size, bits_per_pixel);

    let configured = video
        .SetWidth(size.0)
        .and_then(|_| video.SetHeight(size.1))
        .and_then(|_| video.SetBitrate(bitrate as u32))
        .and_then(|_| video.FrameRate())
        .and_then(|rate| {
            rate.SetNumerator(info.fps.0)?;
            rate.SetDenominator(info.fps.1)
        });

    configured.map_err(|error| winrt_error("Не удалось подобрать параметры кодирования", error))?;

    if !info.has_audio {
        profile
            .SetAudio(None::<&AudioEncodingProperties>)
            .map_err(|error| winrt_error("Не удалось подобрать параметры кодирования", error))?;
    }

    Ok(profile)
}

// Общая часть перекодирования видео.
fn transcode_video(
    source: &Path,
    format: VideoFormat,
    target: PathBuf,
    max_height: Option<u32>,
    bits_per_pixel: f64,
) -> Result<PathBuf, String> {
    let _com = winrt::ComScope::new();

    let file = winrt::storage_file(source)?;

    let info = video_info(&file)?;

    let size = fit_height(&info, max_height);

    let profile = video_profile(format, &info, size, bits_per_pixel)?;

    output::produce_file(&target, |temp| transcode(&file, temp, &profile))
}

pub fn convert_video(source: &Path, format: VideoFormat) -> Result<PathBuf, String> {
    let target = output::next_to(source, format.extension())?;

    transcode_video(source, format, target, None, CONVERT_BITS_PER_PIXEL)
}

// Меньший битрейт и, по параметрам, не выше заданной высоты.
// Если файл не стал заметно меньше — пропуск.
pub fn compress_video(source: &Path, params: &Params) -> Result<PathBuf, ActionError> {
    let max_height = params.compress_height();

    let bits_per_pixel = params.video_bits_per_pixel();

    let original = std::fs::metadata(source)
        .map_err(|error| crate::errors::io("Не удалось прочитать файл", &error))?
        .len();

    // Сначала сравниваем битрейт исходника с тем, к которому сожмём: если
    // он и так не больше, пропускаем сразу, а не после минут перекодирования.
    {
        let _com = winrt::ComScope::new();

        let file = winrt::storage_file(source)?;

        let info = video_info(&file)?;

        let target = video_bitrate(&info, fit_height(&info, max_height), bits_per_pixel)
            + AUDIO_BITRATE;

        let current = file
            .Properties()
            .and_then(|properties| properties.GetVideoPropertiesAsync())
            .and_then(|operation| operation.join())
            .and_then(|properties| properties.Bitrate())
            .unwrap_or(0) as f64;

        if current > 0.0 && current <= target * 1.15 {
            return Err(ActionError::Skipped("уже сжато".to_string()));
        }
    }

    let target = output::next_to_with_suffix(source, "сжато", "mp4")?;

    let result = transcode_video(
        source,
        VideoFormat::Mp4,
        target,
        max_height,
        bits_per_pixel,
    )?;

    let compressed = std::fs::metadata(&result).map(|metadata| metadata.len()).unwrap_or(0);

    if compressed >= original / 100 * 95 {
        let _ = std::fs::remove_file(&result);

        return Err(ActionError::Skipped("уже сжато".to_string()));
    }

    Ok(result)
}

// Уменьшает видео до высоты height (480p, 720p...).
pub fn shrink_video(source: &Path, height: u32) -> Result<PathBuf, ActionError> {
    {
        let _com = winrt::ComScope::new();

        let info = video_info(&winrt::storage_file(source)?)?;

        if info.height <= height {
            return Err(ActionError::Skipped(format!("уже не больше {height}p")));
        }
    }

    let target = output::next_to_with_suffix(source, &format!("{height}p"), "mp4")?;

    Ok(transcode_video(source, VideoFormat::Mp4, target, Some(height), SMALL_BITS_PER_PIXEL)?)
}

pub fn extract_audio(source: &Path, params: &Params) -> Result<PathBuf, String> {
    let _com = winrt::ComScope::new();

    let file = winrt::storage_file(source)?;

    let has_audio = MediaEncodingProfile::CreateFromFileAsync(&file)
        .and_then(|operation| operation.join())
        .map(|profile| profile.Audio().is_ok())
        .unwrap_or(true);

    if !has_audio {
        return Err("В видео нет звука".to_string());
    }

    let profile = AudioFormat::Mp3
        .profile(params)
        .map_err(|error| winrt_error("Не удалось подобрать параметры кодирования", error))?;

    output::produce_file(&output::next_to(source, "mp3")?, |temp| transcode(&file, temp, &profile))
}

pub fn convert_audio(source: &Path, format: AudioFormat, params: &Params) -> Result<PathBuf, String> {
    let _com = winrt::ComScope::new();

    let file = winrt::storage_file(source)?;

    let profile = format.profile(params).map_err(|error| winrt_error("Не удалось подобрать параметры кодирования", error))?;

    output::produce_file(&output::next_to(source, format.extension())?, |temp| {
        transcode(&file, temp, &profile)
    })
}

// Рендер монтажной ленты в файл с прогрессом.
fn render(composition: &MediaComposition, destination: &Path, profile: &MediaEncodingProfile) -> Result<(), String> {
    let destination = winrt::storage_file(destination)?;

    let operation = composition
        .RenderToFileWithProfileAsync(&destination, MediaTrimmingPreference::Precise, profile)
        .map_err(|error| winrt_error("Не удалось склеить", error))?;

    let _ = operation.SetProgress(&AsyncOperationProgressHandler::<TranscodeFailureReason, f64>::new(
        |_, percent| {
            if let Ok(percent) = percent.ok() {
                report_progress(*percent);
            }

            Ok(())
        },
    ));

    wait_for(|| operation.Status(), || {
        let _ = operation.Cancel();
    })?;

    let reason = operation
        .GetResults()
        .map_err(|error| winrt_error("Не удалось склеить", error))?;

    if reason == TranscodeFailureReason::None {
        Ok(())
    } else {
        Err(failure_text(reason))
    }
}

// Склеивает ролики по порядку в один MP4 с параметрами первого.
// Прогресс — по рендерингу: ролики добавляются мгновенно.
pub fn merge_videos(files: &[PathBuf]) -> Result<PathBuf, String> {
    let first = files.first().ok_or("Нет видео")?;

    let _com = winrt::ComScope::new();

    let composition = MediaComposition::new().map_err(|error| winrt_error("Не удалось склеить", error))?;

    let clips = composition.Clips().map_err(|error| winrt_error("Не удалось склеить", error))?;

    for path in files {
        let clip = MediaClip::CreateFromFileAsync(&winrt::storage_file(path)?)
            .and_then(|operation| operation.join())
            .map_err(|error| winrt_error("Не удалось открыть файл", error))?;

        clips.Append(&clip).map_err(|error| winrt_error("Не удалось склеить", error))?;
    }

    let info = video_info(&winrt::storage_file(first)?)?;

    let profile = video_profile(VideoFormat::Mp4, &info, fit_height(&info, None), CONVERT_BITS_PER_PIXEL)?;

    output::produce_file(&output::next_to_with_suffix(first, "склеено", "mp4")?, |temp| {
        render(&composition, temp, &profile)
    })
}

// Склеивает аудиофайлы по порядку. Монтажной ленте Windows нужен видеоряд,
// поэтому под звук подкладывается пустой клип нужной длины, а в файл
// пишется только звук.
pub fn merge_audio(files: &[PathBuf], params: &Params) -> Result<PathBuf, String> {
    let first = files.first().ok_or("Нет аудио")?;

    let format = if crate::file_types::has_extension(first, "m4a") {
        AudioFormat::M4a
    } else {
        AudioFormat::Mp3
    };

    let _com = winrt::ComScope::new();

    let composition = MediaComposition::new().map_err(|error| winrt_error("Не удалось склеить", error))?;

    let tracks = composition
        .BackgroundAudioTracks()
        .map_err(|error| winrt_error("Не удалось склеить", error))?;

    let mut offset = 0.0;

    for path in files {
        let track = BackgroundAudioTrack::CreateFromFileAsync(&winrt::storage_file(path)?)
            .and_then(|operation| operation.join())
            .map_err(|error| winrt_error("Не удалось открыть файл", error))?;

        let duration = seconds(track.OriginalDuration().map_err(|error| winrt_error("Не удалось определить длительность", error))?);

        track
            .SetDelay(span(offset))
            .and_then(|_| tracks.Append(&track))
            .map_err(|error| winrt_error("Не удалось склеить", error))?;

        offset += duration;
    }

    let black = windows::UI::Color { A: 255, R: 0, G: 0, B: 0 };

    let clip = MediaClip::CreateFromColor(black, span(offset)).map_err(|error| winrt_error("Не удалось склеить", error))?;

    composition
        .Clips()
        .and_then(|clips| clips.Append(&clip))
        .map_err(|error| winrt_error("Не удалось склеить", error))?;

    let profile = format.profile(params).map_err(|error| winrt_error("Не удалось подобрать параметры кодирования", error))?;

    output::produce_file(&output::next_to_with_suffix(first, "склеено", format.extension())?, |temp| {
        render(&composition, temp, &profile)
    })
}
