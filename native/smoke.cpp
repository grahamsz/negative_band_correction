// SPDX-License-Identifier: MIT OR Apache-2.0
// Load the actual linked addon and exercise its JavaScript-facing ABI using
// a minimal host API table. This checks linkage/ownership, not Photoshop UI.
#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#endif
#include <chrono>
#include <thread>
#include "UxpAddonShared.h"
#include <iostream>
#include <memory>
#include <stdexcept>
#include <string>
#include <vector>
#include <cstring>
#include <algorithm>

struct addon_value__ {std::string text;std::vector<uint8_t> bytes;addon_callback fn=nullptr;bool buffer=false;};
struct addon_callback_info__ {addon_value args[2];};
static std::vector<std::unique_ptr<addon_value__>> values;
static addon_callback callback=nullptr;
static std::string lastError;
static addon_value make() {values.push_back(std::make_unique<addon_value__>());return values.back().get();}
int main(int argc, char** argv) {
    if(argc!=2)return 2;
#ifdef _WIN32
    auto dll=LoadLibraryA(argv[1]);if(!dll){std::cerr<<"LoadLibrary failed "<<GetLastError();return 1;}
    auto symbol=[&](const char* name){return GetProcAddress(dll,name);};
#else
    auto dll=dlopen(argv[1],RTLD_NOW|RTLD_LOCAL);if(!dll){std::cerr<<dlerror();return 1;}
    auto symbol=[&](const char* name){return dlsym(dll,name);};
#endif
    auto init=reinterpret_cast<uxp_addon_initialize_func>(symbol("uxp_addon_init"));
    using Terminate=void(*)(addon_env);
    auto terminate=reinterpret_cast<Terminate>(symbol("uxp_addon_terminate"));
    if(!init||!terminate)return 1;
    addon_apis api{};
    api.uxp_addon_throw_error=[](addon_env,const char*,const char* s){lastError=s;return addon_ok;};
    api.uxp_addon_create_function=[](addon_env,const char*,size_t,addon_callback cb,void*,addon_value* out){*out=make();(*out)->fn=cb;return addon_ok;};
    api.uxp_addon_set_named_property=[](addon_env,addon_value,const char* name,addon_value v){if(std::string(name)=="dispatch")callback=v->fn;return addon_ok;};
    api.uxp_addon_get_cb_info=[](addon_env,addon_callback_info info,size_t* count,addon_value* args,addon_value*,void**){if(*count<2)return addon_invalid_arg;args[0]=info->args[0];args[1]=info->args[1];*count=2;return addon_ok;};
    api.uxp_addon_get_value_string_utf8=[](addon_env,addon_value v,char* out,size_t cap,size_t* len){*len=v->text.size();if(out&&cap){size_t n=(std::min)(cap-1,v->text.size());memcpy(out,v->text.data(),n);out[n]=0;*len=n;}return addon_ok;};
    api.uxp_addon_get_arraybuffer_info=[](addon_env,addon_value v,void** out,size_t* len){if(!v->buffer)return addon_arraybuffer_expected;*out=v->bytes.data();*len=v->bytes.size();return addon_ok;};
    api.uxp_addon_create_arraybuffer=[](addon_env,size_t n,void** out,addon_value* v){*v=make();(*v)->buffer=true;(*v)->bytes.resize(n);*out=(*v)->bytes.data();return addon_ok;};
    api.uxp_addon_create_string_utf8=[](addon_env,const char* s,size_t n,addon_value* v){*v=make();(*v)->text.assign(s,n);return addon_ok;};
    if(!init(nullptr,make(),std::move(api))||!callback)return 1;
    auto request=make(), pixels=make();pixels->buffer=true;
    addon_callback_info__ info{{request,pixels}};
    request->text=R"({"path":"/health"})";
    auto reply=callback(nullptr,&info);
    if(!reply||reply->text.find("photoshop-banding")==std::string::npos)return 1;
    std::cout<<reply->text<<"\n";
    request->text=R"({"path":"/jobs","method":"POST","body":{"width":256,"height":64,"channels":1}})";
    reply=callback(nullptr,&info);
    if(!reply||reply->text.find("tile_height")==std::string::npos)return 1;
    // Exercise borrowed binary input, background fitting and owned binary output.
    pixels->bytes.resize(256*64*2,0);
    request->text=R"({"path":"/jobs/1/rows/0","method":"PUT"})";
    if(!callback(nullptr,&info))return 1;
    pixels->bytes.clear();
    request->text=R"({"path":"/jobs/1/analyze","method":"POST"})";
    if(!callback(nullptr,&info))return 1;
    bool ready=false;
    for(int attempt=0;attempt<1000;++attempt) {
        request->text=R"({"path":"/jobs/1/status"})";
        reply=callback(nullptr,&info);
        if(!reply)return 1;
        if(reply->text.find("\"state\":\"ready\"")!=std::string::npos){ready=true;break;}
        std::this_thread::sleep_for(std::chrono::milliseconds(10));
    }
    if(!ready)return 1;
    request->text=R"({"path":"/jobs/1/tile/compact-reference/0/64"})";
    reply=callback(nullptr,&info);
    if(!reply||!reply->buffer||reply->bytes.size()!=256*64*2)return 1;
    if(!std::all_of(reply->bytes.begin(),reply->bytes.end(),[](uint8_t v){return v==0;}))return 1;
    request->text="{";
    if(callback(nullptr,&info)!=nullptr||lastError.empty())return 1;
    terminate(nullptr);values.clear();
#ifdef _WIN32
    FreeLibrary(dll);
#else
    dlclose(dll);
#endif
    std::cout<<"Native addon load, capture, fit, binary render, error propagation and unload passed.\n";
}
