use crate::makepad_draw::*;
use ::image::codecs::jpeg::JpegDecoder;
use ::image::codecs::png::PngDecoder;
use ::image::{AnimationDecoder, DynamicImage, ImageDecoder};
use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::io::prelude::*;
use std::io::Cursor;
use std::fs::File;
use std::path::{Path,PathBuf};
use std::cell::RefCell;
use std::sync::Arc;

#[derive(Live, LiveHook, Clone, Copy)]
#[live_ignore]
pub enum ImageFit {
    #[pick] Stretch,
    Horizontal,
    Vertical,
    Smallest,
    Biggest,
    Size
}

impl Default for ImageFit {
    fn default() -> Self {
        ImageFit::Stretch
    }
}

#[derive(Debug, Default, Clone)] 
pub struct ImageBuffer {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u32>,
    pub animation: Option<TextureAnimation>,
}

impl ImageBuffer {
    pub fn new(in_data: &[u8], width: usize, height: usize) -> Result<ImageBuffer, ImageError> {
        let mut out = Vec::new();
        let pixels = width * height;
        out.resize(pixels, 0u32);
        // input pixel packing
        match in_data.len() / pixels {
            4 => for i in 0..pixels {
                let r = in_data[i*4];
                let g = in_data[i*4+1];
                let b = in_data[i*4+2];
                let a = in_data[i*4+3];
                out[i] = ((a as u32)<<24) | ((r as u32)<<16) | ((g as u32)<<8) | ((b as u32)<<0);
            }
            3 => for i in 0..pixels {
                let r = in_data[i*3];
                let g = in_data[i*3+1];
                let b = in_data[i*3+2];
                out[i] = 0xff000000 | ((r as u32)<<16) | ((g as u32)<<8) | ((b as u32)<<0);
            }
            2 => for i in 0..pixels {
                let r = in_data[i*2];
                let a = in_data[i*2+1];
                out[i] = ((a as u32)<<24) | ((r as u32)<<16) | ((r as u32)<<8) | ((r as u32)<<0);
            }
            1 => for i in 0..pixels {
                let r = in_data[i];
                out[i] = ((0xff as u32)<<24) | ((r as u32)<<16) | ((r as u32)<<8) | ((r as u32)<<0);
            }
            unsupported => {
                return Err(ImageError::InvalidPixelAlignment(unsupported));
            }     
        }
        Ok(ImageBuffer {
            width,
            height,
            data: out,
            animation: None
        })
    }
    
    pub fn into_new_texture(self, cx:&mut Cx)->Texture{
        let texture = Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
            width: self.width,
            height: self.height,
            data: Some(self.data),
            updated: TextureUpdated::Full,
        });
        texture.set_animation(cx, self.animation);
        texture
    }
    
    pub fn from_png(data: &[u8]) -> Result<Self, ImageError> {
        let decoder = PngDecoder::new(Cursor::new(data)).map_err(ImageError::PngDecode)?;
        if decoder.is_apng().map_err(ImageError::PngDecode)? {
            return Self::decode_animated_png(decoder);
        }
        let image = DynamicImage::from_decoder(decoder).map_err(ImageError::PngDecode)?;
        Self::from_decoded(image)
    }

    /// Packs every composited APNG frame left to right, then top to bottom, into one
    /// texture as wide as `Cx::max_texture_width` allows, with one spare frame row.
    fn decode_animated_png(decoder: PngDecoder<Cursor<&[u8]>>) -> Result<ImageBuffer, ImageError> {
        let (width, height) = decoder.dimensions();
        let (width, height) = (width as usize, height as usize);
        let fits_horizontal = Cx::max_texture_width() / width;
        let total_width = fits_horizontal * width;
        let mut data = Vec::new();
        let mut num_frames = 0;
        let (mut x0, mut y0) = (0, 0);
        let frames = decoder.apng().map_err(ImageError::PngDecode)?.into_frames();
        for frame in frames {
            let frame = frame.map_err(ImageError::PngDecode)?;
            let rgba = frame.buffer().as_raw();
            data.resize(data.len().max((y0 + height) * total_width), 0);
            for y in 0..height {
                let row = &rgba[y * width * 4..(y + 1) * width * 4];
                let out = &mut data[(y0 + y) * total_width + x0..][..width];
                for (out, p) in out.iter_mut().zip(row.chunks_exact(4)) {
                    *out = ((p[3] as u32)<<24) | ((p[0] as u32)<<16) | ((p[1] as u32)<<8) | (p[2] as u32);
                }
            }
            num_frames += 1;
            x0 += width;
            if x0 >= total_width {
                y0 += height;
                x0 = 0
            }
        }
        let total_height = (num_frames / fits_horizontal + 1) * height;
        data.resize(total_width * total_height, 0);
        Ok(ImageBuffer {
            width: total_width,
            height: total_height,
            data,
            animation: Some(TextureAnimation {
                width,
                height,
                num_frames,
            }),
        })
    }

    pub fn from_jpg(data: &[u8]) -> Result<Self, ImageError> {
        let decoder = JpegDecoder::new(Cursor::new(data)).map_err(ImageError::JpgDecode)?;
        let image = DynamicImage::from_decoder(decoder).map_err(ImageError::JpgDecode)?;
        Self::from_decoded(image)
    }

    /// Packs 8-bit gray, gray-alpha, RGB and RGBA directly; other depths go through RGBA8.
    fn from_decoded(image: DynamicImage) -> Result<Self, ImageError> {
        let (width, height) = (image.width() as usize, image.height() as usize);
        match image {
            DynamicImage::ImageLuma8(_)
            | DynamicImage::ImageLumaA8(_)
            | DynamicImage::ImageRgb8(_)
            | DynamicImage::ImageRgba8(_) => Self::new(image.as_bytes(), width, height),
            image => Self::new(image.into_rgba8().as_raw(), width, height),
        }
    }
}

pub enum ImageCacheEntry{
    Loaded(Texture),
    Loading(usize, usize),
}

#[derive(Debug)]
pub struct AsyncImageLoad{
    pub image_path: PathBuf,
    pub result: RefCell<Option<Result<ImageBuffer, ImageError>>>
}

pub struct ImageCache {
    map: HashMap<PathBuf, ImageCacheEntry>,
    pub thread_pool: Option<TagThreadPool<PathBuf>>,
}

impl ImageCache {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            thread_pool: None,
        }
    }
}


/// The possible errors that can occur when loading or creating an image texture.
#[derive(Debug)]
pub enum ImageError {
    /// The image data buffer was empty or otherwise invalid.
    EmptyData,
    /// The image's pixel data was not aligned to 3-byte or 4-byte pixels.
    /// The unsupported alignment value (in bytes) is included.
    InvalidPixelAlignment(usize),
    /// The image data could not be decoded as a JPEG.
    JpgDecode(::image::ImageError),
    /// The image file at the given resource path could not be found.
    PathNotFound(PathBuf),
    /// The image data could not be decoded as a PNG.
    PngDecode(::image::ImageError),
    /// The image data was in an unsupported format.
    /// Currently, only JPEG and PNG are supported.
    UnsupportedFormat,
}

pub enum AsyncLoadResult{
    Loading(usize,usize),
    Loaded,
}

impl Error for ImageError {}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

pub trait ImageCacheImpl {
    fn get_texture(&self, id:usize) -> &Option<Texture>;
    fn set_texture(&mut self, texture: Option<Texture>,id: usize);
    
    fn lazy_create_image_cache(&mut self,cx: &mut Cx) {
        if !cx.has_global::<ImageCache>() {
            cx.set_global(ImageCache::new());
        }
    }

    fn load_png_from_data(&mut self, cx: &mut Cx, data: &[u8], id:usize) -> Result<(), ImageError> {
        match ImageBuffer::from_png(&*data){
            Ok(data)=>{
                self.set_texture(Some(data.into_new_texture(cx)), id);
                Ok(())
            }
            Err(err)=>{
                Err(err)
            }
        }
    }
    
    fn load_jpg_from_data(&mut self, cx: &mut Cx, data: &[u8], id:usize) -> Result<(), ImageError> {
        match ImageBuffer::from_jpg(&*data){
            Ok(data)=>{
                self.set_texture(Some(data.into_new_texture(cx)), id);
                Ok(())
            }
            Err(err)=>{
                Err(err)
            }
        }
    }
    
    fn image_size_by_data(data:&[u8], image_path:&Path)-> Result<(usize,usize), ImageError> {
        let (width, height) = if image_path.extension().map(|s| s == "jpg").unwrap_or(false) {
            JpegDecoder::new(Cursor::new(data)).map_err(ImageError::JpgDecode)?.dimensions()
        }
        else if image_path.extension().map(|s| s == "png").unwrap_or(false) {
            PngDecoder::new(Cursor::new(data)).map_err(ImageError::PngDecode)?.dimensions()
        } else {
            return Err(ImageError::UnsupportedFormat)
        };
        Ok((width as usize, height as usize))
    }
    
    fn image_size_by_path(image_path:&Path)-> Result<(usize,usize), ImageError> {
        if let Ok(mut f) = File::open(image_path){
            let mut data = vec![0u8;1024]; // yolo chunk size
            match f.read(&mut data) {
                Ok(_len) => {
                    Self::image_size_by_data(&data, image_path)
                }
                Err(err) => {
                    error!("load_image_file_by_path: Resource not found {:?} {}", image_path, err);
                    return Err(ImageError::PathNotFound(image_path.into()))
                }
            }
        }
        else{
            error!("load_image_file_by_path: File not found {:?}", image_path);
            return Err(ImageError::PathNotFound(image_path.into()))
        }
    }
        
    fn process_async_image_load(&mut self, cx:&mut Cx, image_path: &Path, result: Result<ImageBuffer, ImageError>)->bool{
        // alright now we should stuff this thing into our cache
        if let Ok(data) = result{
            let texture = data.into_new_texture(cx);
            cx.get_global::<ImageCache>().map.insert(image_path.into(), ImageCacheEntry::Loaded(texture.clone()));
        }
        false
    }
    
    fn load_image_from_cache(&mut self, cx:&mut Cx, image_path: &Path, id: usize)->bool{
        if let Some(texture) = cx.get_global::<ImageCache>().map.get(image_path){
            match texture{
                ImageCacheEntry::Loaded(texture)=>{
                    self.set_texture(Some(texture.clone()), id);
                    return true
                }
                _=>()
            }
        }
        false
    }
    
    fn load_image_from_data_async_impl(
        &mut self,
        cx: &mut Cx,
        image_path: &Path,
        data: Arc<Vec<u8>>,
        id: usize,
    ) -> Result<AsyncLoadResult, ImageError> {
        if let Some(texture) = cx.get_global::<ImageCache>().map.get(image_path){
            match texture{
                ImageCacheEntry::Loaded(texture)=>{
                    let texture = texture.clone();
                    // lets fetch the texture size
                    //let (_w,_h) = texture.get_format(cx).vec_width_height().unwrap_or((100,100));
                    self.set_texture(Some(texture), id);
                    Ok(AsyncLoadResult::Loaded)
                }
                ImageCacheEntry::Loading(w,h)=>{
                    Ok(AsyncLoadResult::Loading(*w, *h))
                }
            }
        }
        else{
            if  cx.get_global::<ImageCache>().thread_pool.is_none(){
                cx.get_global::<ImageCache>().thread_pool = Some(TagThreadPool::new(cx, cx.cpu_cores().max(3) - 2))
            }
            let (w,h) = Self::image_size_by_data(&*data, image_path)?;
            // open image file and read the headers
            cx.get_global::<ImageCache>().map.insert(image_path.into(), ImageCacheEntry::Loading(w,h));
                        
            cx.get_global::<ImageCache>().thread_pool.as_mut().unwrap().execute_rev(image_path.into(), move |image_path|{
                if image_path.extension().map(|s| s == "jpg").unwrap_or(false) {
                    match ImageBuffer::from_jpg(&*data){
                        Ok(data)=>{
                            Cx::post_action(AsyncImageLoad{
                                image_path, 
                                result: RefCell::new(Some(Ok(data)))
                            });
                        }
                        Err(err)=>{
                            Cx::post_action(AsyncImageLoad{
                                image_path, 
                                result: RefCell::new(Some(Err(err)))
                            });
                        }
                    }
                } else if image_path.extension().map(|s| s == "png").unwrap_or(false) {
                    match ImageBuffer::from_png(&*data){
                        Ok(data)=>{
                            Cx::post_action(AsyncImageLoad{
                                image_path, 
                                result: RefCell::new(Some(Ok(data)))
                            });
                        }
                        Err(err)=>{
                            Cx::post_action(AsyncImageLoad{
                                image_path, 
                                result: RefCell::new(Some(Err(err)))
                            });
                        }
                    }
                } else {
                    Cx::post_action(AsyncImageLoad{
                        image_path, 
                        result: RefCell::new(Some(Err(ImageError::UnsupportedFormat)))
                    });
                }
            });
            Ok(AsyncLoadResult::Loading(w, h))
        }
    }
    
    fn load_image_file_by_path_async_impl(
        &mut self,
        cx: &mut Cx,
        image_path: &Path,
        id: usize,
    ) -> Result<AsyncLoadResult, ImageError> {
        if let Some(texture) = cx.get_global::<ImageCache>().map.get(image_path){
            match texture{
                ImageCacheEntry::Loaded(texture)=>{
                    let texture = texture.clone();
                    // lets fetch the texture size
                    //let (_w,_h) = texture.get_format(cx).vec_width_height().unwrap_or((100,100));
                    self.set_texture(Some(texture), id);
                    Ok(AsyncLoadResult::Loaded)
                }
                ImageCacheEntry::Loading(w,h)=>{
                    Ok(AsyncLoadResult::Loading(*w, *h))
                }
            }
        }
        else{
            if  cx.get_global::<ImageCache>().thread_pool.is_none(){
                 cx.get_global::<ImageCache>().thread_pool = Some(TagThreadPool::new(cx, cx.cpu_cores().max(3) - 2))
            }
            let (w,h) = Self::image_size_by_path(image_path)?;
            // open image file and read the headers
            cx.get_global::<ImageCache>().map.insert(image_path.into(), ImageCacheEntry::Loading(w,h));
            
            cx.get_global::<ImageCache>().thread_pool.as_mut().unwrap().execute_rev(image_path.into(), move |image_path|{
                if let Ok(mut f) = File::open(&image_path){
                    let mut data = Vec::new();
                    match f.read_to_end(&mut data) {
                        Ok(_len) => {        
                            if image_path.extension().map(|s| s == "jpg").unwrap_or(false) {
                                match ImageBuffer::from_jpg(&*data){
                                    Ok(data)=>{
                                        Cx::post_action(AsyncImageLoad{
                                            image_path, 
                                            result: RefCell::new(Some(Ok(data)))
                                        });
                                    }
                                    Err(err)=>{
                                        Cx::post_action(AsyncImageLoad{
                                            image_path, 
                                            result: RefCell::new(Some(Err(err)))
                                        });
                                    }
                                }
                            } else if image_path.extension().map(|s| s == "png").unwrap_or(false) {
                                match ImageBuffer::from_png(&*data){
                                    Ok(data)=>{
                                        Cx::post_action(AsyncImageLoad{
                                            image_path, 
                                            result: RefCell::new(Some(Ok(data)))
                                        });
                                    }
                                    Err(err)=>{
                                        Cx::post_action(AsyncImageLoad{
                                            image_path, 
                                            result: RefCell::new(Some(Err(err)))
                                        });
                                    }
                                }
                            } else {
                                Cx::post_action(AsyncImageLoad{
                                    image_path, 
                                    result: RefCell::new(Some(Err(ImageError::UnsupportedFormat)))
                                });
                            }
                        }
                        Err(_err) => {
                            Cx::post_action(AsyncImageLoad{
                                image_path: image_path.clone(), 
                                result: RefCell::new(Some(Err(ImageError::PathNotFound(image_path))))
                            });
                        }
                    }
                }
                else{
                    Cx::post_action(AsyncImageLoad{
                        image_path: image_path.clone(), 
                        result: RefCell::new(Some(Err(ImageError::PathNotFound(image_path))))
                    });
                }
            });
            Ok(AsyncLoadResult::Loading(w, h))
        }
    }
    
    fn load_image_file_by_path_and_data(&mut self, cx:&mut Cx, data:&[u8], id:usize, image_path:&Path)-> Result<(), ImageError> {
        if image_path.extension().map(|s| s == "jpg").unwrap_or(false) {
            match ImageBuffer::from_jpg(&*data){
                Ok(data)=>{
                    let texture = data.into_new_texture(cx);
                    cx.get_global::<ImageCache>().map.insert(image_path.into(), ImageCacheEntry::Loaded(texture.clone()));
                    self.set_texture(Some(texture), id);
                    Ok(())
                }
                Err(err)=>{
                    error!("load_image_file_by_path_and_data: Cannot load jpeg image from path: {:?} {}", image_path, err);
                    Err(err)
                }
            }
        } else if image_path.extension().map(|s| s == "png").unwrap_or(false) {
            match ImageBuffer::from_png(&*data){
                Ok(data)=>{
                    let texture = data.into_new_texture(cx);
                    cx.get_global::<ImageCache>().map.insert(image_path.into(), ImageCacheEntry::Loaded(texture.clone()));
                    self.set_texture(Some(texture), id);
                    Ok(())
                }
                Err(err)=>{
                    error!("load_image_file_by_path_and_data: Cannot load png image from path: {:?} {}", image_path, err);
                    Err(err)
                }
            }
        } else {
            error!("load_image_file_by_path_and_data: Image format not supported {:?}", image_path);
            Err(ImageError::UnsupportedFormat)
        }
    }
        
    fn load_image_file_by_path(
        &mut self,
        cx: &mut Cx,
        image_path: &Path,
        id: usize,
    ) -> Result<(), ImageError> {
        if let Some(ImageCacheEntry::Loaded(texture)) = cx.get_global::<ImageCache>().map.get(image_path){
            self.set_texture(Some(texture.clone()), id);
            Ok(())
        }
        else{
            if let Ok(mut f) = File::open(image_path){
                let mut data = Vec::new();
                match f.read_to_end(&mut data) {
                    Ok(_len) => {
                        self.load_image_file_by_path_and_data(cx, &data, id, image_path)
                    }
                    Err(err) => {
                        error!("load_image_file_by_path: Resource not found {:?} {}", image_path, err);
                        Err(ImageError::PathNotFound(image_path.into()))
                    }
                }
            }
            else{
                error!("load_image_file_by_path: File not found {:?}", image_path);
                Err(ImageError::PathNotFound(image_path.into()))
            }
        }
    }
    
    fn load_image_dep_by_path(
        &mut self,
        cx: &mut Cx,
        image_path: &str,
        id: usize,
    ) -> Result<(), ImageError> {
        let p_image_path = Path::new(image_path);
        if let Some(ImageCacheEntry::Loaded(texture)) = cx.get_global::<ImageCache>().map.get(p_image_path){
            self.set_texture(Some(texture.clone()), id);
            Ok(())
        } 
        else{
            match cx.take_dependency(image_path) {
                Ok(data) => {
                    self.load_image_file_by_path_and_data(cx, &data, id, p_image_path)
                }
                Err(err) => {
                    error!("load_image_dep_by_path: Resource not found {} {}", image_path, err);
                    Err(ImageError::PathNotFound(image_path.into()))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ImageBuffer, ImageError};

    #[test]
    fn png_preserves_alpha_and_argb_packing() {
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0xf4, 0x22, 0x7f,
            0x8a, 0x00, 0x00, 0x00, 0x0f, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
            0x1f, 0x08, 0x1b, 0x00, 0x10, 0x79, 0x03, 0x7e, 0x7d, 0x63, 0xce, 0xd7, 0x00, 0x00, 0x00, 0x00,
            0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let image = ImageBuffer::from_png(&png).unwrap();
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.data, [0xffff0000, 0x8000ff00]);
        assert!(image.animation.is_none());
    }

    #[test]
    fn apng_composes_subframes_and_disposes_background() {
        // Red/blue canvas; half-alpha green over blue; then clear that subframe
        // and replace red with opaque green. The decoder must return full frames.
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0xf4, 0x22,
            0x7f, 0x8a, 0x00, 0x00, 0x00, 0x08, 0x61, 0x63, 0x54, 0x4c, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00,
            0x00, 0x00, 0xce, 0xed, 0xba, 0xc0, 0x00, 0x00, 0x00, 0x1a, 0x66, 0x63, 0x54, 0x4c, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x0a, 0x00, 0x00, 0xf9, 0x29, 0xb6, 0x79, 0x00, 0x00, 0x00, 0x0e,
            0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x42, 0xff, 0x01, 0x0f, 0xf9,
            0x03, 0xfd, 0x85, 0x11, 0x99, 0x76, 0x00, 0x00, 0x00, 0x1a, 0x66, 0x63, 0x54, 0x4c, 0x00, 0x00,
            0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x0a, 0x01, 0x01, 0x6e, 0x9e, 0x04, 0x13, 0x00, 0x00, 0x00, 0x11,
            0x66, 0x64, 0x41, 0x54, 0x00, 0x00, 0x00, 0x02, 0x78, 0x9c, 0x63, 0x60, 0xf8, 0xcf, 0xd0, 0x00,
            0x00, 0x03, 0x82, 0x01, 0x80, 0xe1, 0xc4, 0x91, 0x83, 0x00, 0x00, 0x00, 0x1a, 0x66, 0x63, 0x54,
            0x4c, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x0a, 0x00, 0x00, 0x2c, 0x9a, 0x09, 0xed, 0x00,
            0x00, 0x00, 0x11, 0x66, 0x64, 0x41, 0x54, 0x00, 0x00, 0x00, 0x04, 0x78, 0x9c, 0x63, 0x60, 0xf8,
            0xcf, 0xf0, 0x1f, 0x00, 0x04, 0x01, 0x01, 0xff, 0x9a, 0x25, 0xa2, 0x2a, 0x00, 0x00, 0x00, 0x00,
            0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        let image = ImageBuffer::from_png(&png).unwrap();
        let animation = image.animation.unwrap();
        assert_eq!((animation.width, animation.height, animation.num_frames), (2, 1, 3));
        assert_eq!(&image.data[..3], &[0xffff0000, 0xff0000ff, 0xffff0000]);
        let blended = image.data[3];
        // The decoder composites in floating point before quantizing each channel to u8.
        for (shift, expected) in [(24, 255), (16, 0), (8, 128), (0, 127)] {
            assert!(((blended >> shift) & 255).abs_diff(expected) <= 1);
        }
        assert_eq!(&image.data[4..6], &[0xff00ff00, 0]);
    }

    #[test]
    fn jpeg_decodes_rgb_and_reports_corrupt_headers() {
        let mut jpeg = Vec::new();
        ::image::codecs::jpeg::JpegEncoder::new(&mut jpeg)
            .encode(&[255, 255, 255], 1, 1, ::image::ExtendedColorType::Rgb8)
            .unwrap();
        let image = ImageBuffer::from_jpg(&jpeg).unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.data, [0xffffffff]);
        assert!(matches!(
            ImageBuffer::from_jpg(b"\xff\xd8invalid"),
            Err(ImageError::JpgDecode(_))
        ));
    }
}
