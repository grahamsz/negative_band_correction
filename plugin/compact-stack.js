"use strict";
async function writeCompactStack(ps,request,job,report,spec,context,check,timings,putStrip,decode16,onStatus) {
    const doc=ps.app.activeDocument,{BlendMode,ElementPlacement}=ps.constants;
    const components=report.pure_components;
    if(!components?.length) throw new Error("No supported locally fitted wave components were found.");
    const originalTop=spec.sourceLayer,layers=[],structure=[];
    let stage="Creating locally fitted wave layers", effectiveOpacity;
    try {
        for(const component of components) {
            check();
            stage=`Creating wave ${component.name}`;onStatus(stage+"...");
            const layer=await doc.createPixelLayer({name:`Wave ${component.name} - editable density mask`,blendMode:BlendMode.LINEARLIGHT,opacity:spec.initial_opacity});
            if(!layer) throw new Error("Photoshop did not create the wave layer.");
            layer.visible=false;
            // Photoshop stores opacity on a 0..255 scale. Use the host's actual
            // value, not 0.66, when normalizing the mask and predicting the blend.
            const actual=Number(layer.opacity)/100;
            if(!Number.isFinite(actual)||actual<=0||actual>1) throw new Error("Photoshop returned an invalid layer opacity.");
            if(effectiveOpacity===undefined) {
                effectiveOpacity=actual;
                await request(`/jobs/${job.id}/compact-opacity`,"POST",{opacity:actual});
                report.compact_opacity={requested_percent:spec.initial_opacity,effective_fraction:actual};
                if(report.config) report.config.compact_opacity=actual;
            } else if(Math.abs(actual-effectiveOpacity)>1e-10) throw new Error("Photoshop applied inconsistent opacity across wave layers.");
            const anchor=layers.length?layers[layers.length-1]:originalTop;
            if(anchor) await layer.move(anchor,ElementPlacement.PLACEBEFORE);
            layers.push(layer);structure.push({component:component.index,layer_id:layer.id,opacity_percent:layer.opacity});
            for(let top=0;top<spec.height;top+=job.tile_height) {
                check();
                const rows=Math.min(job.tile_height,spec.height-top),pixels=spec.width*rows,count=pixels*spec.channels;
                stage=`Rendering wave ${component.name}, rows ${top+1}-${top+rows}`;
                let tick=Date.now();
                const data=decode16(await request(`/jobs/${job.id}/tile/compact-${component.index}/${top}/${rows}`,"GET",undefined,true),count+pixels);
                timings.render_ms+=Date.now()-tick;tick=Date.now();
                stage=`Writing wave ${component.name}, rows ${top+1}-${top+rows}`;
                await putStrip(ps,{...spec,layerID:layer.id,top,rows,fullRange:false},data.subarray(0,count));
                stage=`Writing density mask for ${component.name}, rows ${top+1}-${top+rows}`;
                await putStrip(ps,{...spec,layerID:layer.id,top,rows},data.subarray(count),true);
                timings.write_ms+=Date.now()-tick;
                context.reportProgress({value:0.4+0.5*(layers.length-1+(top+rows)/spec.height)/components.length,commandName:`${spec.bandSummary} — Creating correction`});
            }
        }
        report.compact_layer_structure=structure;
        if(layers.length===1) {layers[0].isClippingMask=true;return layers[0];}
        stage="Grouping completed locally fitted wave layers";
        const root=await doc.createLayerGroup({name:"Band correction - locally fitted waves",fromLayers:layers,blendMode:BlendMode.PASSTHROUGH,opacity:100});
        if(!root) throw new Error("Photoshop did not create the correction group.");
        root.visible=false;
        if(originalTop) await root.move(originalTop,ElementPlacement.PLACEBEFORE);
        root.isClippingMask=true;
        for(const layer of layers) layer.visible=true;
        return root;
    } catch(error) {throw new Error(stage+": "+(error?.message || String(error)));}
}
module.exports={writeCompactStack};
