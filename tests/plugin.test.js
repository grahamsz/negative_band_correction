"use strict";
const {test}=require("node:test"), assert=require("node:assert/strict");
const {parseSettings,decode16,encode16,run,detectedSpacing}=require("../plugin/workflow.js");
const {createNativeClient}=require("../plugin/native-client.js");
const input={strength:"80",darkFull:"10",darkOff:"60",boost:"50",roi:""};
test("settings preserve accepted defaults and reject invalid numbers/ROIs",()=>{
    assert.deepEqual(parseSettings(input),{options:{strength:.8,dark_full:.1,dark_off:.6},carrier_boost:.5,initial_opacity:66});
    for(const value of [{strength:"NaN"},{darkOff:"10"},{boost:"49"},{roi:"0,2,9,1"},{strength:""}]) assert.throws(()=>parseSettings({...input,...value}));
    assert.deepEqual(parseSettings({...input,roi:"10,200,4,80"}).options.detection_roi,[10,200,4,80]);
});
test("wire samples have explicit little endian order and exact size",()=>{
    const values=new Uint16Array([0,256,32768,65535]);
    assert.deepEqual([...new Uint8Array(encode16(values))],[0,0,0,1,0,128,255,255]);
    assert.deepEqual(decode16(encode16(values),4),values);
    assert.throws(()=>decode16(new ArrayBuffer(3),2));
    assert.deepEqual(decode16(encode16(values.subarray(1,3)),2),new Uint16Array([256,32768]));
});
test("native transport borrows only binary pixels and propagates errors",async()=>{
    let sent;
    const pixels=new Uint16Array([17,65535]).buffer;
    const client=createNativeClient({dispatch:(message,buffer)=>{sent={message:JSON.parse(message),buffer};return '{"ok":true}';}});
    assert.deepEqual(await client('/jobs/1/rows/0','PUT',pixels),{ok:true});
    assert.equal(sent.buffer,pixels);
    assert.deepEqual(sent.message,{path:'/jobs/1/rows/0',method:'PUT',body:null});
    await client('/jobs','POST',{width:100});
    assert.equal(sent.buffer.byteLength,0);assert.deepEqual(sent.message.body,{width:100});
    const binary=createNativeClient({dispatch:()=>pixels});
    assert.equal(await binary('/tile','GET',undefined,true),pixels);
    await assert.rejects(binary('/status'));
    await assert.rejects(client('/tile','GET',undefined,true));
    await assert.rejects(createNativeClient({dispatch:()=>{throw new Error('native failure');}})('/health'),/native failure/);
});
function harness({failWrite=false,failMask=false,mismatch=false,cancel=false}={}) {
    const calls=[], resumes=[], layers=[];
    let created;
    const create=async(options,group=false)=>{created={id:layers.length+2,visible:true,group,...options,opacity:Math.round((options.opacity??100)*255/100)*100/255};const layer=created;layer.move=async(target,placement)=>{assert.ok(target,"Move target must exist");layer.parent=placement==="inside"?target:target.parent;calls.push(["move",layer.id,target.id,placement]);};layers.push(layer);if(group) {assert.ok(options.fromLayers?.length,"Pure groups must be built from explicit existing layers");for(const child of options.fromLayers)child.parent=layer;}return layer;};
    const source={id:1,name:"Background",visible:true,kind:"pixel"}, other={id:99,name:"Curves",visible:true,kind:"adjustment"}; const doc={id:7,width:48,height:32,resolution:4800,bitsPerChannel:16,mode:"rgb",layers:[other,source],activeLayers:[source],
        createPixelLayer:options=>create(options),createLayerGroup:options=>create(options,true)};
    const ps={app:{documents:[doc],activeDocument:doc},constants:{BitsPerChannelType:{SIXTEEN:16},DocumentMode:{RGB:"rgb",GRAYSCALE:"gray"},
        LayerKind:{GROUP:"group",NORMAL:"pixel"},BlendMode:{LINEARLIGHT:"linearLight",NORMAL:"normal",PASSTHROUGH:"passThrough"},ElementPlacement:{PLACEBEFORE:"before",PLACEINSIDE:"inside"}},
        core:{executeAsModal:async fn=>fn({isCancelled:cancel,reportProgress:value=>calls.push(["progress",value]),hostControl:{suspendHistory:async()=>42,resumeHistory:async(id,commit)=>resumes.push([id,commit])}})},
        imaging:{getPixels:async args=>{
            calls.push(["getPixels",args]); if(args.layerID===undefined) assert.equal(other.visible,false,"Unrelated layers must be isolated during verification"); else assert.equal(args.layerID,source.id); const rows=args.sourceBounds.bottom-args.sourceBounds.top;
            return {level:0,sourceBounds:args.sourceBounds,imageData:{width:48,height:rows,components:3,componentSize:16,colorProfile:"Adobe RGB (1998)",
                getData:async()=>new Uint16Array(48*rows*3).fill(mismatch&&created&&created.visible?10100:10000),dispose:()=>{}}};
        },createImageDataFromBuffer:async(data,options)=>({data,options,dispose:()=>{}}),
        putPixels:async args=>{calls.push(["putPixels",args]);if(failWrite)throw new Error("write failure");},putLayerMask:async args=>{calls.push(["putLayerMask",args]);if(failMask)throw new Error("mask failure");}}};
    const request=async(path,method="GET",body)=>{
        calls.push([path,method,body]);
        if(path==="/jobs")return {id:9,tile_height:16};
        if(path.endsWith("/status"))return {state:"ready",report:{layers:[{name:"test"}],pure_components:[{index:0,name:"Red / 37px wave",channel:0,period_px:37},{index:1,name:"Blue / 42px wave",channel:2,period_px:42}]}};
        if(path.includes("/tile/")) {
            const parts=path.split("/"),rows=Number(parts[6]);
            if(parts[4]==="compact-reference")return encode16(new Uint16Array(48*rows*3).fill(10000));
            if(parts[4].startsWith("compact-")) return encode16(new Uint16Array(48*rows*4).fill(16384));
            throw new Error("Unexpected tile kind: "+parts[4]);
        }
        return {};
    };
    return {ps,request,calls,resumes,layers,created:()=>created};
}
test("one compact pure wave creates exactly one Linear Light layer and one mask",async()=>{
    const h=harness();const request=async(...args)=>{const v=await h.request(...args);if(args[0].endsWith('/status'))v.report.pure_components=v.report.pure_components.slice(0,1);return v;};
    const report=await run(h.ps,request,parseSettings({...input}));
    assert.equal(h.layers.length,1);assert.equal(h.layers[0].blendMode,"linearLight");
    assert.equal(h.calls.filter(c=>c[0]==="putPixels").length,2);
    assert.equal(h.calls.filter(c=>c[0]==="putLayerMask").length,2);
    assert.equal(report.photoshop_verification.reference,"compact-reference");
    assert.equal(h.layers[0].opacity,168/255*100);
    assert.equal(h.calls.find(c=>c[0].endsWith("/compact-opacity"))[2].opacity,h.layers[0].opacity/100);
    assert.deepEqual(report.compact_opacity,{requested_percent:66,effective_fraction:h.layers[0].opacity/100});
    assert.deepEqual(h.resumes,[[42,true]]);
});
test("multiple compact waves keep independent masks with no neutral backing layers",async()=>{
    const h=harness();const report=await run(h.ps,h.request,parseSettings({...input,output:"compact"}));
    assert.equal(h.layers.filter(l=>!l.group).length,2);assert.equal(h.layers.filter(l=>l.group).length,1);
    const group=h.layers.find(l=>l.group);assert.equal(group.blendMode,"passThrough");
    for(const layer of h.layers.filter(l=>!l.group)) {assert.equal(layer.parent,group);assert.equal(layer.blendMode,"linearLight");}
    assert.equal(report.compact_layer_structure.length,2);assert.deepEqual(h.resumes,[[42,true]]);
});
test("compact mask failures and host mismatches roll back the whole correction",async()=>{
    for(const options of [{failWrite:true},{failMask:true},{mismatch:true}]) {
        const h=harness(options);await assert.rejects(run(h.ps,h.request,parseSettings({...input,output:"compact"})));
        assert.deepEqual(h.resumes,[[42,false]]);assert.deepEqual(h.calls.at(-1).slice(0,2),["/jobs/9","DELETE"]);
    }
});

test("initial opacity is validated and is independent of correction strength",()=>{
    const settings=parseSettings({...input,output:"compact"});
    assert.equal(settings.initial_opacity,66);assert.equal(settings.options.strength,.8);
    assert.equal(parseSettings({...input,output:"compact",initialOpacity:"100"}).initial_opacity,100);
    for(const value of ["", "0", "101", "NaN"]) assert.throws(()=>parseSettings({...input,output:"compact",initialOpacity:value}));
});

test("cancelled runs and scans without bands add no layers",async()=>{
    const cancelled=harness({cancel:true});await assert.rejects(run(cancelled.ps,cancelled.request,parseSettings(input)));
    assert.equal(cancelled.layers.length,0);
    const h=harness();const request=async(...args)=>{const result=await h.request(...args);if(args[0].endsWith('/status')) result.report.pure_components=[];return result;};
    await run(h.ps,request,parseSettings(input));assert.equal(h.layers.length,0);assert.equal(h.resumes.length,0);
    assert.deepEqual(h.calls.at(-1).slice(0,2),["/jobs/9","DELETE"]);
});
test("panel has no alternate-output selector or dormant workflow dependency",()=>{
    const fs=require('node:fs');
    assert.doesNotMatch(fs.readFileSync(require.resolve('../plugin/index.html'),'utf8'),/<select|id="output"/);
    assert.doesNotMatch(fs.readFileSync(require.resolve('../plugin/workflow.js'),'utf8'),/settings\.output|writePureStack|Corrected image layer/);
});
test("selected source pixels, layer name and spacing are used; visibility is restored",async()=>{
    const h=harness(), messages=[], detected=[];
    const report=await run(h.ps,h.request,parseSettings(input),v=>messages.push(v),(v,dpi)=>detected.push([v,dpi]));
    const reads=h.calls.filter(c=>c[0]==="getPixels");
    assert.equal(reads.filter(c=>c[1].layerID===1).length,2);
    assert.equal(reads.filter(c=>c[1].layerID===undefined).length,16);
    assert.ok(messages.includes("Reading 'Background'"));
    assert.ok(h.calls.some(c=>c[0]==="progress"&&c[1].commandName==="Reading 'Background'"));
    assert.deepEqual(report.source_layer,{id:1,name:"Background"});
    assert.equal(detected[0][1],4800);
    assert.match(detected[0][0],/37.0 px \/ 0.196 mm/);
    assert.ok(h.calls.some(c=>c[0]==="move"&&c[2]===1));
    assert.equal(h.layers.at(-1).isClippingMask,true);
    assert.equal(h.ps.app.activeDocument.layers[0].visible,true);
});
test("verification failure restores originally hidden and visible layers",async()=>{
    const h=harness({mismatch:true});
    h.ps.app.activeDocument.activeLayers[0].visible=false;
    await assert.rejects(run(h.ps,h.request,parseSettings(input)),/differs/);
    assert.equal(h.ps.app.activeDocument.activeLayers[0].visible,false);
    assert.equal(h.ps.app.activeDocument.layers[0].visible,true);
});
test("multiple selection is rejected before reading or creating layers",async()=>{
    const h=harness();h.ps.app.activeDocument.activeLayers.push({id:10});
    await assert.rejects(run(h.ps,h.request,parseSettings(input)),/exactly one/);
    assert.equal(h.calls.length,0);
});
test("nested selected layers keep their parent and restore ancestor visibility",async()=>{
    const h=harness(), doc=h.ps.app.activeDocument, source=doc.activeLayers[0];
    const parent={id:70,kind:"group",visible:false,layers:[source],parent:null};
    source.parent=parent;doc.layers=[doc.layers[0],parent];
    const request=async(...args)=>{const v=await h.request(...args);if(args[0].endsWith('/status'))v.report.pure_components=v.report.pure_components.slice(0,1);return v;};
    await run(h.ps,request,parseSettings(input));
    assert.equal(h.layers[0].parent,parent);
    assert.equal(parent.visible,false);
    assert.equal(source.visible,true);
    assert.equal(h.layers[0].isClippingMask,true);
});
test("spacing uses document resolution and never invents a DPI",()=>{
    assert.equal(detectedSpacing([{period_px:800}],4800),"800.0 px / 4.233 mm");
    assert.equal(detectedSpacing([{period_px:800}],3200),"800.0 px / 6.350 mm");
    assert.equal(detectedSpacing([{period_px:800}],0),"800.0 px / mm unavailable");
    assert.equal(detectedSpacing([],4800),"No coherent band detected");
});
test("panel has inline settings and no report button or bottom diagnostic box",()=>{
    const fs=require("node:fs"), html=fs.readFileSync(require.resolve("../plugin/index.html"),"utf8");
    assert.doesNotMatch(html,/id="report"|id="connection"/);
    assert.match(html,/<label><span>Correction strength/);
    assert.match(html,/id="detected"/);
    assert.doesNotMatch(html.slice(html.indexOf('class="actions"')),/id="status"/);
});
