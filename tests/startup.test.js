"use strict";
const {test}=require("node:test");
const assert=require("node:assert/strict");
const fs=require("node:fs");
const vm=require("node:vm");
const source=fs.readFileSync(require.resolve("../plugin/main.js"),"utf8");

function startPanel(addon) {
    const elements=new Map();
    const element=id=>{
        if(!elements.has(id)) elements.set(id,{disabled:false,textContent:"",addEventListener() {}});
        return elements.get(id);
    };
    const finished=vm.runInNewContext(source,{
        document:{getElementById:element},
        require:name=>{
            if(name==="banding-v010.uxpaddon") return addon;
            if(name==="./native-client.js") return require("../plugin/native-client.js");
            if(name==="./workflow.js") return {};
            if(name==="photoshop" || name==="uxp") return {};
            throw new Error("Unexpected module: "+name);
        }
    });
    return {element,finished};
}

test("panel waits for asynchronous native loading before enabling correction",async()=>{
    let resolve;
    const h=startPanel(new Promise(r=>{resolve=r;}));
    assert.equal(h.element("apply").disabled,true);
    resolve({dispatch:message=>{
        assert.equal(JSON.parse(message).path,"/health");
        return JSON.stringify({service:"photoshop-banding",protocol:1,version:"0.3.0"});
    }});
    await h.finished;
    assert.equal(h.element("status").textContent,"Select an image layer to begin.");
    assert.equal(h.element("apply").disabled,false);


});

test("native load failures retain their cause and keep correction disabled",async()=>{
    const cases=[
        [()=>Promise.reject(new Error("Adobe loader failure")),/Adobe loader failure/],
        [()=>Promise.resolve({}),/Native banding module did not load/],
        [()=>Promise.resolve({dispatch:()=>JSON.stringify({service:"wrong",protocol:1})}),/Incompatible bundled Rust engine/]
    ];
    for(const [load,message] of cases) {
        const h=startPanel(load());
        await h.finished;

        assert.match(h.element("status").textContent,message);
        assert.equal(h.element("apply").disabled,true);
    
    }
});