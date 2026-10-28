// The CLAP entry of every format: the three functions `apf::export_plugin!` defines in the
// Rust static library, gathered into the exported `clap_entry` symbol.

#include <clap/clap.h>

extern "C"
{
  bool apf_entry_init(const char *plugin_path);
  void apf_entry_deinit(void);
  const void *apf_entry_get_factory(const char *factory_id);

#ifdef __GNUC__
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wattributes"
#endif

  CLAP_EXPORT extern const clap_plugin_entry clap_entry = {CLAP_VERSION, apf_entry_init, apf_entry_deinit,
                                                           apf_entry_get_factory};

#ifdef __GNUC__
#pragma GCC diagnostic pop
#endif
}
