"use strict";
const {writeCompactStack}=require("./compact-stack.js");
function parseSettings(input) {
    const number = (key) => {
        if (String(input[key]).trim() === "") throw new Error(`Enter ${key}.`);
        const value = Number(input[key]);
        if (!Number.isFinite(value)) throw new Error(`Invalid ${key}.`);
        return value / 100;
    };
    const strength = number("strength"), dark_full = number("darkFull"), dark_off = number("darkOff"), carrier_boost = number("boost");
    if (!(strength >= 0 && strength <= 1 && dark_full >= 0 && dark_full < dark_off && dark_off <= 1 && carrier_boost >= .5 && carrier_boost <= 3)) {
        throw new Error("Use strength 0–100%, signal 50–300%, and 0 ≤ full-mask brightness < cutoff ≤ 100%.");
    }
    const options = {strength, dark_full, dark_off};
    if (String(input.roi || "").trim()) {
        const roi = input.roi.split(",").map(v => v.trim());
        if (roi.length !== 4 || roi.some(v => !/^\d+$/.test(v))) throw new Error("ROI must be X0,X1,Y0,Y1 in original pixels.");
        options.detection_roi = roi.map(Number);
        const [x0,x1,y0,y1] = options.detection_roi;
        if (!(x0 < x1 && y0 < y1) || options.detection_roi.some(v => !Number.isSafeInteger(v))) throw new Error("ROI bounds must increase.");
    }
    const opacity=Number(input.initialOpacity ?? 66);
    if(!Number.isFinite(opacity)||opacity<1||opacity>100) throw new Error("Use initial layer opacity between 1% and 100%.");
    return {options,carrier_boost,initial_opacity:opacity};
}
const LITTLE_ENDIAN = new Uint8Array(new Uint16Array([1]).buffer)[0] === 1;
function decode16(buffer, expected) {
    if (buffer.byteLength !== expected*2) throw new Error("Rust engine returned an incomplete pixel strip.");
    if (LITTLE_ENDIAN) return new Uint16Array(buffer);
    const data = new Uint16Array(expected), view = new DataView(buffer);
    for (let i=0;i<expected;i++) data[i] = view.getUint16(i*2,true);
    return data;
}
function encode16(data) {
    if (LITTLE_ENDIAN) return data.byteOffset === 0 && data.byteLength === data.buffer.byteLength
        ? data.buffer : data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength);
    const result = new ArrayBuffer(data.length*2), view = new DataView(result);
    for (let i=0;i<data.length;i++) view.setUint16(i*2,data[i],true);
    return result;
}
async function readStrip(ps, documentID, width, top, rows, channels, layerID) {
    const image = await ps.imaging.getPixels({documentID,...(layerID===undefined?{}:{layerID}),sourceBounds:{left:0,top,right:width,bottom:top+rows},componentSize:16,applyAlpha:false});
    const pixels = image.imageData;
    try {
        if (image.level !== 0 || pixels.width !== width || pixels.height !== rows || image.sourceBounds.left !== 0 || image.sourceBounds.top !== top) {
            throw new Error("Select an opaque, full-canvas image layer. Photoshop returned cropped or scaled pixels.");
        }
        if (pixels.componentSize !== 16 || ![channels,channels+1].includes(pixels.components)) throw new Error("Unexpected source pixel format.");
        const data = await pixels.getData({chunky:true,fullRange:true});
        if (!(data instanceof Uint16Array) || data.length !== width*rows*pixels.components) throw new Error("Invalid source pixel buffer.");
        if (pixels.components === channels) return {data,profile:pixels.colorProfile};
        const opaque = new Uint16Array(width*rows*channels);
        for (let p=0;p<width*rows;p++) {
            if (data[p*pixels.components+channels] !== 65535) throw new Error("Select an opaque image layer. Transparent pixels are unsupported.");
            for(let c=0;c<channels;c++) opaque[p*channels+c]=data[p*pixels.components+c];
        }
        return {data:opaque,profile:pixels.colorProfile};
    } finally { pixels.dispose(); }
}
async function putStrip(ps, spec, data, mask = false) {
    const options = {width:spec.width,height:spec.rows,components:mask?1:spec.channels,chunky:true,
        colorSpace:mask?"Grayscale":spec.colorSpace,fullRange:mask?false:spec.fullRange};
    if (!mask && spec.profile) options.colorProfile = spec.profile;
    const imageData = await ps.imaging.createImageDataFromBuffer(data,options);
    try {
        const args = {documentID:spec.documentID,layerID:spec.layerID,imageData,targetBounds:{left:0,top:spec.top},replace:spec.top===0};
        if (mask) await ps.imaging.putLayerMask(args); else await ps.imaging.putPixels(args);
    } finally { imageData.dispose(); }
}
function detectedSpacing(components, resolution) {
    const dpi=Number(resolution), seen=new Set();
    const values=[];
    for(const component of components || []) {
        const px=Number(component.period_px);
        if(!Number.isFinite(px)||px<=0) continue;
        const key=px.toFixed(1);
        if(seen.has(key)) continue;
        seen.add(key);
        values.push(`${key} px / ${Number.isFinite(dpi)&&dpi>0?(px*25.4/dpi).toFixed(3)+" mm":"mm unavailable"}`);
    }
    return values.length?values.join("; "):"No coherent band detected";
}
function originalVisibility(doc, source, groupKind) {
    const keep=new Set([source.id]);
    for(let parent=source.parent;parent;parent=parent.parent) keep.add(parent.id);
    const entries=[];
    const visit=layers=>{for(const layer of layers) {
        entries.push({layer,visible:layer.visible,keep:keep.has(layer.id)});
        if(groupKind!==undefined && layer.kind===groupKind) visit(layer.layers);
    }};
    visit(doc.layers);
    return entries;
}
async function run(ps, request, settings, onStatus = () => {}, onDetected = () => {}) {
    const {app, constants, core} = ps;
    if (!app.documents.length) throw new Error("Open an uninverted negative first.");
    const doc = app.activeDocument;
    const selected=Array.from(doc.activeLayers || []);
    if(selected.length!==1) throw new Error("Select exactly one image layer to correct.");
    const sourceLayer=selected[0];
    if((constants.LayerKind && sourceLayer.kind===constants.LayerKind.GROUP) || sourceLayer.isClippingMask) throw new Error("Select an image layer, not a group or clipped layer.");
    const sourceName=String(sourceLayer.name || "Untitled layer");
    const visibility=originalVisibility(doc,sourceLayer,constants.LayerKind?.GROUP);
    if (doc.bitsPerChannel !== constants.BitsPerChannelType.SIXTEEN) throw new Error("Use a 16-bit document (Image > Mode > 16 Bits/Channel).");
    const channels = doc.mode === constants.DocumentMode.RGB ? 3 : doc.mode === constants.DocumentMode.GRAYSCALE ? 1 : 0;
    if (!channels) throw new Error("Use RGB or grayscale mode.");
    const width=Number(doc.width), height=Number(doc.height), documentID=doc.id;
    let job, report, cleanupWarning;
    const started=Date.now(), timings={read_ms:0,capture_ms:0,analysis_ms:0,render_ms:0,write_ms:0,verify_ms:0};
    try {
        await core.executeAsModal(async context => {
            let history;
            const check = () => { if(context.isCancelled) throw new Error("Correction cancelled."); };
            context.onCancel = () => { if(job) request(`/jobs/${job.id}`,"DELETE").catch(()=>{}); };
            try {
                check();
                job = await request("/jobs","POST",{width,height,channels,options:settings.options,carrier_boost:settings.carrier_boost});
                let profile;
                onStatus(`Reading '${sourceName}'`);
                context.reportProgress({value:0,commandName:`Reading '${sourceName}'`});
                for(let top=0;top<height;top+=job.tile_height) {
                    check();
                    const rows=Math.min(job.tile_height,height-top);
                    let tick=Date.now();
                    const source=await readStrip(ps,documentID,width,top,rows,channels,sourceLayer.id);
                    timings.read_ms+=Date.now()-tick;
                    if(profile !== undefined && profile !== source.profile) throw new Error("Document profile changed during capture.");
                    profile=source.profile;
                    tick=Date.now();
                    await request(`/jobs/${job.id}/rows/${top}`,"PUT",encode16(source.data));
                    timings.capture_ms+=Date.now()-tick;
                    context.reportProgress({value:0.3*(top+rows)/height,commandName:`Reading '${sourceName}'`});
                }
                onStatus("Finding coherent bands and estimating local intensity…");
                const fitStarted=Date.now();
                await request(`/jobs/${job.id}/analyze`,"POST",{});
                for(;;) {
                    check();
                    const state=await request(`/jobs/${job.id}/status`);
                    if(state.state === "failed") throw new Error(state.error);
                    if(state.state === "ready") {report=state.report;break;}
                    context.reportProgress({value:0.35,commandName:"Fitting bands and refining signed residuals"});
                    await new Promise(resolve=>setTimeout(resolve,300));
                }
                timings.analysis_ms=Date.now()-fitStarted;
                const bandSummary=detectedSpacing(report.pure_components,doc.resolution);
                report.source_layer={id:sourceLayer.id,name:sourceName};
                report.document_resolution_ppi=Number(doc.resolution);
                onDetected(bandSummary,Number(doc.resolution));
                report.output_mode="compact";
                report.diagnostics_target="Detector diagnostics describe the initial exponential model; residual_refinement describes the refined fit and photoshop_verification checks the final locally fitted wave composite.";
                report.active_masks={combined:"Estimated debanded density and darkness with a phase-independent headroom reserve; layer pixels contain fitted local waves and image-dependent blend compensation"};
                if(!report.pure_components?.length) {onStatus("No coherent band signal found. No layer was added.");return;}
                check();
                history=await context.hostControl.suspendHistory({documentID,name:"Reduce negative banding"});
                const spec={documentID,width,height,channels,sourceLayer,bandSummary,initial_opacity:settings.initial_opacity,colorSpace:channels===3?"RGB":"Grayscale",profile};
                const layer=await writeCompactStack(ps,request,job,report,spec,context,check,timings,putStrip,decode16,onStatus);
                onStatus("Checking the Photoshop composite against the Rust prediction...");
                const verifyStarted=Date.now();
                layer.visible=true;
                // Test the actual host composite, not just a simulated blend equation.
                let maxDifference=0, checkedSamples=0;
                const expectedKind="compact-reference";
                const checkRows=Array.from(new Set(Array.from({length:16},(_,i)=>Math.floor(i*(height-1)/15))));
                try {
                // Verify only the selected source plus its correction. Other
                // layers (including adjustments above it) are restored even
                // on cancellation or a failed host comparison.
                for(const entry of visibility) entry.layer.visible=entry.keep;
                for(const top of checkRows) {
                    check();
                    const actual=(await readStrip(ps,documentID,width,top,1,channels)).data;
                    const expected=decode16(await request(`/jobs/${job.id}/tile/${expectedKind}/${top}/1`,"GET",undefined,true),width*channels);
                    for(let i=0;i<actual.length;i++) maxDifference=Math.max(maxDifference,Math.abs(actual[i]-expected[i]));
                    checkedSamples+=actual.length;
                }
                } finally {
                    for(const entry of visibility) entry.layer.visible=entry.visible;
                }
                report.photoshop_verification={max_difference_full_range:maxDifference,samples:checkedSamples,rows:checkRows,
                    reference:expectedKind,output:"compact",document_id:documentID,layer_id:layer.id,profile};
                timings.verify_ms=Date.now()-verifyStarted;
                report.timings_ms={...timings,total_ms:Date.now()-started};
                const perChannel=Array.from({length:channels},(_,c)=>report.pure_components.filter(p=>p.channel===c).length);
                const tolerance=8*Math.max(1,...perChannel);
                report.photoshop_verification.tolerance_full_range=tolerance;
                if(maxDifference>tolerance) throw new Error(`Photoshop's composite differs from the ${expectedKind} prediction by ${maxDifference}/65535. The new layers were rolled back. Check the document's blending gamma settings; custom gamma can affect Linear Light.`);
                await context.hostControl.resumeHistory(history,true);history=undefined;
                context.reportProgress({value:1,commandName:"Correction complete"});
                onStatus(`Correction created for '${sourceName}'.`);
            } catch(error) {
                if(history !== undefined) {
                    try {await context.hostControl.resumeHistory(history,false);}
                    catch(rollbackError) {throw new Error((error?.message || String(error))+" Rollback also failed: "+(rollbackError?.message || String(rollbackError)));}
                }
                throw error;
            }
        },{commandName:"Reduce negative banding"});
        return report;
    } finally {
        if(job) {try {await request(`/jobs/${job.id}`,"DELETE");} catch(error) {cleanupWarning=error.message;}}
        if(cleanupWarning) onStatus(`Engine cleanup could not finish: ${cleanupWarning}. Restart the engine to release the temporary job.`);
    }
}
module.exports={parseSettings,decode16,encode16,readStrip,putStrip,run,detectedSpacing};
