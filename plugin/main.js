"use strict";
const ps=require("photoshop");
const {createNativeClient}=require("./native-client.js");
const {parseSettings,run}=require("./workflow.js");
let client;
const el=id=>document.getElementById(id);
const status=message=>{el("status").textContent=message;};
function busy(value) {
    el("apply").disabled=value || !client;
    for(const id of ["strength","initialOpacity","darkFull","darkOff","boost","roi"]) el(id).disabled=value;
}
el("apply").addEventListener("click",async()=>{
    busy(true);el("detected").textContent="Not measured yet";el("resolution").textContent="";
    try {
        if(!client) throw new Error("The bundled engine could not load. Reinstall the plugin and reopen the panel.");
        const input={};for(const key of ["strength","initialOpacity","darkFull","darkOff","boost","roi"]) input[key]=el(key).value;
        await run(ps,client,parseSettings(input),status,(spacing,dpi)=>{
            el("detected").textContent=spacing;
            el("resolution").textContent=Number.isFinite(dpi)&&dpi>0?`Based on document resolution: ${dpi} ppi`:"Document resolution unavailable";
        });
    } catch(error) {status(error.message);} finally {busy(false);}
});
(async()=>{
    busy(true);status("Loading engine...");
    try {
        client=createNativeClient(await require("banding-v010.uxpaddon"));
        const health=await client("/health");
        if(health.service!=="photoshop-banding" || health.protocol!==1) throw new Error("Incompatible bundled Rust engine.");
        status("Select an image layer to begin.");
    } catch(error) {client=undefined;status(`Could not load the bundled engine: ${error.message}`);}
    finally {busy(false);}
})();